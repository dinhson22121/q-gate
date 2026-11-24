use std::sync::Arc;

use futures::future::BoxFuture;

use super::prompt::{AUDIT_SYSTEM_PROMPT, audit_user_message};
use super::reviewer::{reviewer_error, spent};
use super::schema::{AuditAnswer, audit_schema};
use crate::llm::StructuredModel;
use crate::review::application::ports::{AuditInput, AuditOutput, FindingAuditor, ReviewerError};

pub struct ClaudeAuditor {
    model: Arc<dyn StructuredModel>,
}

impl ClaudeAuditor {
    pub fn new(model: Arc<dyn StructuredModel>) -> Self {
        Self { model }
    }
}

impl FindingAuditor for ClaudeAuditor {
    fn audit<'a>(&'a self, input: AuditInput<'a>) -> BoxFuture<'a, Result<AuditOutput, ReviewerError>> {
        Box::pin(async move {
            let user = audit_user_message(input.findings, input.files);
            let answer = self
                .model
                .structured::<AuditAnswer>(AUDIT_SYSTEM_PROMPT, &user, &audit_schema())
                .await
                .map_err(reviewer_error)?;
            let (usage, cost_usd) = spent(&answer);
            Ok(AuditOutput { audits: answer.value.into_audits(), usage, cost_usd })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::{FakeModel, LlmError};
    use crate::review::domain::change_set::fixtures::{added, file};
    use crate::review::domain::finding::Severity;
    use crate::review::domain::finding::fixtures::finding;
    use crate::review::domain::verification::AuditVerdict;

    async fn audit_with(fake: &Arc<FakeModel>) -> Result<AuditOutput, ReviewerError> {
        let files = vec![file("app.py", 1, vec![added(1, "eval(user_input)")], Some("eval(user_input)\n"))];
        let findings = vec![finding("app.py", 1, Severity::Major, "eval(user_input)")];
        ClaudeAuditor::new(fake.clone()).audit(AuditInput { files: &files, findings: &findings }).await
    }

    #[tokio::test]
    async fn audit_parses_verdicts_and_reports_usage_and_cost() {
        let fake = Arc::new(FakeModel::answering(json!({"audits": [{
            "index": 0, "verdict": "contradicted", "counter_file": "app.py", "counter_line": 1,
            "counter_quote": "eval(user_input)", "reason": "r"
        }]})));

        let output = audit_with(&fake).await.unwrap();

        assert_eq!(output.audits.len(), 1);
        assert_eq!(output.audits[0].verdict, AuditVerdict::Contradicted);
        assert_eq!((output.usage.input_tokens, output.usage.output_tokens), (100, 10));
        assert_eq!(output.cost_usd, Some(0.5));
    }

    #[tokio::test]
    async fn audit_sends_instructions_findings_diff_and_schema() {
        let fake = Arc::new(FakeModel::answering(json!({"audits": []})));

        audit_with(&fake).await.unwrap();

        let calls = fake.calls.lock().unwrap();
        let (system, user, schema) = &calls[0];
        assert_eq!(system, AUDIT_SYSTEM_PROMPT);
        assert!(user.contains("<finding index=\"0\"") && user.contains("1 + eval(user_input)"));
        assert_eq!(schema, &audit_schema());
    }

    #[tokio::test]
    async fn llm_errors_map_to_reviewer_errors() {
        let fake = Arc::new(FakeModel::new(vec![Err(LlmError::Truncated)]));
        assert_eq!(audit_with(&fake).await.unwrap_err(), ReviewerError::Truncated);
        let fake = Arc::new(FakeModel::new(vec![Err(LlmError::Rejected("no".into()))]));
        assert_eq!(audit_with(&fake).await.unwrap_err(), ReviewerError::Rejected("no".into()));
    }
}
