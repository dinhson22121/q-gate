use std::path::Path;

use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::llm::{LlmError, Structured};

pub trait StructuredModel: Send + Sync {
    fn structured_json<'a>(
        &'a self,
        system: &'a str,
        user_prompt: &'a str,
        schema: &'a Value,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>>;

    fn structured_json_with_files<'a>(
        &'a self,
        _system: &'a str,
        _user_prompt: &'a str,
        _schema: &'a Value,
        _share_dir: &'a Path,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
        Box::pin(async { Err(LlmError::Unsupported("this model cannot read shared files".into())) })
    }

    fn preflight(&self) -> BoxFuture<'_, Result<(), LlmError>> {
        Box::pin(async { Ok(()) })
    }
}

impl dyn StructuredModel + '_ {
    pub async fn structured<T: DeserializeOwned>(
        &self,
        system: &str,
        user_prompt: &str,
        schema: &Value,
    ) -> Result<Structured<T>, LlmError> {
        parse_with_retry(user_prompt, |prompt| {
            Box::pin(async move { self.structured_json(system, &prompt, schema).await })
        })
        .await
    }

    pub async fn structured_with_files<T: DeserializeOwned>(
        &self,
        system: &str,
        user_prompt: &str,
        schema: &Value,
        share_dir: &Path,
    ) -> Result<Structured<T>, LlmError> {
        parse_with_retry(user_prompt, |prompt| {
            Box::pin(async move { self.structured_json_with_files(system, &prompt, schema, share_dir).await })
        })
        .await
    }
}

async fn parse_with_retry<'a, T, F>(user_prompt: &str, call: F) -> Result<Structured<T>, LlmError>
where
    T: DeserializeOwned,
    F: Fn(String) -> BoxFuture<'a, Result<Structured<Value>, LlmError>>,
{
    let first = call(user_prompt.to_owned()).await?;
    let reason = match serde_json::from_value::<T>(first.value) {
        Ok(value) => return Ok(Structured { value, usage: first.usage, cost_usd: first.cost_usd }),
        Err(e) => e.to_string(),
    };
    let retry_prompt = format!(
        "{user_prompt}\n\nYour previous answer was rejected ({reason}). Answer again, strictly matching the schema."
    );
    let second = call(retry_prompt).await?;
    let (mut usage, cost_usd) = (first.usage, first.cost_usd + second.cost_usd);
    usage += second.usage;
    let value = serde_json::from_value(second.value)
        .map_err(|e| LlmError::InvalidResponse(format!("answer does not match the schema: {e}")))?;
    Ok(Structured { value, usage, cost_usd })
}

#[cfg(test)]
pub(crate) mod fake {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::llm::Usage;

    pub struct FakeModel {
        replies: Mutex<VecDeque<Result<Value, LlmError>>>,
        pub calls: Mutex<Vec<(String, String, Value)>>,
        pub shared_dirs: Mutex<Vec<std::path::PathBuf>>,
    }

    impl FakeModel {
        pub fn new(replies: Vec<Result<Value, LlmError>>) -> Self {
            Self { replies: Mutex::new(replies.into()), calls: Mutex::default(), shared_dirs: Mutex::default() }
        }

        pub fn answering(answer: Value) -> Self {
            Self::new(vec![Ok(answer)])
        }

        pub fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    fn scripted(value: Value) -> Structured<Value> {
        Structured { value, usage: Usage { input_tokens: 100, output_tokens: 10, ..Usage::default() }, cost_usd: 0.5 }
    }

    impl StructuredModel for FakeModel {
        fn structured_json<'a>(
            &'a self,
            system: &'a str,
            user_prompt: &'a str,
            schema: &'a Value,
        ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
            self.calls.lock().unwrap().push((system.into(), user_prompt.into(), schema.clone()));
            let reply = self.replies.lock().unwrap().pop_front().expect("unexpected extra model call");
            Box::pin(async move { reply.map(scripted) })
        }

        fn structured_json_with_files<'a>(
            &'a self,
            system: &'a str,
            user_prompt: &'a str,
            schema: &'a Value,
            share_dir: &'a Path,
        ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
            self.shared_dirs.lock().unwrap().push(share_dir.to_path_buf());
            self.structured_json(system, user_prompt, schema)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::fake::FakeModel;
    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Answer {
        n: u32,
    }

    #[tokio::test]
    async fn parses_the_answer_into_the_requested_type() {
        let fake = FakeModel::answering(json!({"n": 3}));
        let model: &dyn StructuredModel = &fake;
        let out = model.structured::<Answer>("sys", "user", &json!({})).await.unwrap();
        assert_eq!(out.value, Answer { n: 3 });
        assert_eq!(fake.call_count(), 1);
    }

    #[tokio::test]
    async fn retries_once_on_a_mismatch_and_sums_usage_and_cost() {
        let fake = FakeModel::new(vec![Ok(json!({"n": "x"})), Ok(json!({"n": 4}))]);
        let model: &dyn StructuredModel = &fake;
        let out = model.structured::<Answer>("sys", "user", &json!({})).await.unwrap();
        assert_eq!(out.value, Answer { n: 4 });
        assert_eq!((out.usage.input_tokens, out.cost_usd), (200, 1.0));
        let calls = fake.calls.lock().unwrap();
        assert!(calls[1].1.contains("previous answer was rejected"));
    }

    #[tokio::test]
    async fn a_second_mismatch_is_an_invalid_response() {
        let fake = FakeModel::new(vec![Ok(json!({})), Ok(json!({}))]);
        let model: &dyn StructuredModel = &fake;
        let err = model.structured::<Answer>("s", "u", &json!({})).await.unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn transport_errors_are_not_retried() {
        let fake = FakeModel::new(vec![Err(LlmError::Unavailable("down".into()))]);
        let model: &dyn StructuredModel = &fake;
        let err = model.structured::<Answer>("s", "u", &json!({})).await.unwrap_err();
        assert_eq!(err, LlmError::Unavailable("down".into()));
        assert_eq!(fake.call_count(), 1);
    }
}
