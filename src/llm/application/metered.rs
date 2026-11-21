use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use serde_json::Value;

use super::StructuredModel;
use crate::llm::{LlmError, Structured, Usage};

#[derive(Debug, Default)]
struct Totals {
    usage: Usage,
    cost_usd: f64,
}

#[derive(Debug, Clone, Default)]
pub struct UsageMeter {
    totals: Arc<Mutex<Totals>>,
}

impl UsageMeter {
    pub fn usage(&self) -> Usage {
        self.totals.lock().unwrap_or_else(PoisonError::into_inner).usage
    }

    pub fn cost_usd(&self) -> f64 {
        self.totals.lock().unwrap_or_else(PoisonError::into_inner).cost_usd
    }

    fn record(&self, usage: Usage, cost_usd: f64) {
        let mut totals = self.totals.lock().unwrap_or_else(PoisonError::into_inner);
        totals.usage += usage;
        totals.cost_usd += cost_usd;
    }
}

pub struct MeteredModel {
    inner: Arc<dyn StructuredModel>,
    meter: UsageMeter,
}

impl MeteredModel {
    pub fn new(inner: Arc<dyn StructuredModel>, meter: UsageMeter) -> Self {
        Self { inner, meter }
    }
}

impl StructuredModel for MeteredModel {
    fn structured_json<'a>(
        &'a self,
        system: &'a str,
        user_prompt: &'a str,
        schema: &'a Value,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
        Box::pin(async move {
            let answer = self.inner.structured_json(system, user_prompt, schema).await?;
            self.meter.record(answer.usage, answer.cost_usd);
            Ok(answer)
        })
    }

    fn structured_json_with_files<'a>(
        &'a self,
        system: &'a str,
        user_prompt: &'a str,
        schema: &'a Value,
        share_dir: &'a Path,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
        Box::pin(async move {
            let answer = self.inner.structured_json_with_files(system, user_prompt, schema, share_dir).await?;
            self.meter.record(answer.usage, answer.cost_usd);
            Ok(answer)
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::FakeModel;

    #[tokio::test]
    async fn sums_successful_calls_and_skips_failures() {
        let fake = FakeModel::new(vec![Ok(json!(1)), Err(LlmError::Unavailable("x".into())), Ok(json!(2))]);
        let meter = UsageMeter::default();
        let model = MeteredModel::new(Arc::new(fake), meter.clone());
        let schema = json!({});
        assert!(model.structured_json("s", "u", &schema).await.is_ok());
        assert!(model.structured_json("s", "u", &schema).await.is_err());
        assert!(model.structured_json("s", "u", &schema).await.is_ok());
        assert_eq!((meter.usage().input_tokens, meter.usage().output_tokens, meter.cost_usd()), (200, 20, 1.0));
    }
}
