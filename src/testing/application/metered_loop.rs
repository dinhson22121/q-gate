use crate::llm::{ContentPart, LlmError, LoopLimits, LoopOutcome, SystemBlock, ToolHandler, ToolSpec, UsageMeter};

use super::ports::ToolLoop;

pub struct MeteredToolLoop<L> {
    inner: L,
    meter: UsageMeter,
}

impl<L> MeteredToolLoop<L> {
    pub fn new(inner: L, meter: UsageMeter) -> Self {
        Self { inner, meter }
    }
}

impl<L: ToolLoop> ToolLoop for MeteredToolLoop<L> {
    async fn drive<H: ToolHandler>(
        &self,
        system: &[SystemBlock],
        user: Vec<ContentPart>,
        tools: &[ToolSpec],
        handler: &H,
        limits: LoopLimits,
    ) -> Result<LoopOutcome, LlmError> {
        let outcome = self.inner.drive(system, user, tools, handler, limits).await?;
        self.meter.record(outcome.usage, outcome.cost_usd);
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::{LoopStop, ToolOutput, Usage};

    struct Billed {
        result: Result<LoopOutcome, LlmError>,
    }

    impl ToolLoop for Billed {
        async fn drive<H: ToolHandler>(
            &self,
            _system: &[SystemBlock],
            _user: Vec<ContentPart>,
            _tools: &[ToolSpec],
            _handler: &H,
            _limits: LoopLimits,
        ) -> Result<LoopOutcome, LlmError> {
            self.result.clone()
        }
    }

    struct Idle;

    impl ToolHandler for Idle {
        async fn call(&self, _name: &str, _input: serde_json::Value) -> ToolOutput {
            ToolOutput::ok(json!(null).to_string())
        }

        fn finished(&self) -> bool {
            true
        }
    }

    fn outcome(input_tokens: u64, cost_usd: f64) -> LoopOutcome {
        LoopOutcome {
            turns: 1,
            stop: LoopStop::Finished,
            final_text: String::new(),
            usage: Usage { input_tokens, output_tokens: 5, ..Usage::default() },
            cost_usd,
        }
    }

    async fn drive(driver: &MeteredToolLoop<Billed>) -> Result<LoopOutcome, LlmError> {
        driver.drive(&[], Vec::new(), &[], &Idle, LoopLimits { max_turns: 1 }).await
    }

    #[tokio::test]
    async fn every_finished_session_adds_its_usage_and_cost_to_the_meter() {
        let meter = UsageMeter::default();
        let driver = MeteredToolLoop::new(Billed { result: Ok(outcome(10, 0.25)) }, meter.clone());

        drive(&driver).await.unwrap();
        drive(&driver).await.unwrap();

        assert_eq!(meter.usage().input_tokens, 20);
        assert_eq!(meter.usage().output_tokens, 10);
        assert!((meter.cost_usd() - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn a_failed_session_leaves_the_meter_untouched() {
        let meter = UsageMeter::default();
        let driver = MeteredToolLoop::new(Billed { result: Err(LlmError::Unavailable("down".into())) }, meter.clone());

        assert!(drive(&driver).await.is_err());

        assert_eq!(meter.usage(), Usage::default());
        assert!(meter.cost_usd().abs() < f64::EPSILON);
    }
}
