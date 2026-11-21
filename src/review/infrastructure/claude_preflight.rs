use std::sync::Arc;

use futures::future::BoxFuture;

use crate::llm::{LlmError, StructuredModel, USAGE_LIMIT_PREFIX, tokens_for_chars};
use crate::review::application::{ModelPreflight, ProbeFailure};

pub struct ClaudePreflight {
    model: Arc<dyn StructuredModel>,
}

impl ClaudePreflight {
    pub fn new(model: Arc<dyn StructuredModel>) -> Self {
        Self { model }
    }
}

impl ModelPreflight for ClaudePreflight {
    fn probe(&self) -> BoxFuture<'_, Result<(), ProbeFailure>> {
        Box::pin(async move {
            self.model.preflight().await.map_err(|error| match error {
                LlmError::RateLimited { resets_at, .. } => ProbeFailure::RateLimited { resets_at },
                other => ProbeFailure::Failed(other.to_string()),
            })
        })
    }

    fn usage_limit_marker(&self) -> &str {
        USAGE_LIMIT_PREFIX
    }

    fn tokens_for_chars(&self, chars: usize) -> u64 {
        tokens_for_chars(chars)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::llm::Structured;

    struct Probe(Result<(), LlmError>);

    impl StructuredModel for Probe {
        fn structured_json<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: &'a Value,
        ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
            unreachable!("preflight never asks a question")
        }

        fn preflight(&self) -> BoxFuture<'_, Result<(), LlmError>> {
            let result = self.0.clone();
            Box::pin(async move { result })
        }
    }

    async fn probe(result: Result<(), LlmError>) -> Result<(), ProbeFailure> {
        ClaudePreflight::new(Arc::new(Probe(result))).probe().await
    }

    #[tokio::test]
    async fn rate_limits_keep_their_reset_time_and_other_errors_keep_their_text() {
        let limited = LlmError::RateLimited { resets_at: Some("4:30pm".into()), message: "m".into() };
        assert_eq!(probe(Err(limited)).await, Err(ProbeFailure::RateLimited { resets_at: Some("4:30pm".into()) }));
        let failed = probe(Err(LlmError::Unavailable("down".into()))).await;
        assert!(matches!(failed, Err(ProbeFailure::Failed(text)) if text.contains("down")));
        assert_eq!(probe(Ok(())).await, Ok(()));
    }

    #[test]
    fn marker_and_token_heuristic_come_from_the_llm_context() {
        let adapter = ClaudePreflight::new(Arc::new(Probe(Ok(()))));
        assert_eq!(adapter.usage_limit_marker(), USAGE_LIMIT_PREFIX);
        assert_eq!(adapter.tokens_for_chars(9), tokens_for_chars(9));
    }
}
