use std::sync::Arc;

use super::prompt::{
    REQUIREMENTS_SYSTEM_PROMPT, REVIEW_SYSTEM_PROMPT, guidelines_block, requirements_user_message, review_user_message,
};
use super::schema::{AssessmentAnswer, ReviewAnswer, assessment_schema, review_schema};
use crate::llm::{LlmError, Structured, StructuredModel, Usage};
use crate::review::application::ports::{
    AssessmentOutput, CodeReviewer, RequirementsInput, ReviewInput, ReviewerError, ReviewerOutput,
};
use crate::review::domain::report::TokenUsage;

pub struct ClaudeReviewer {
    model: Arc<dyn StructuredModel>,
}

impl ClaudeReviewer {
    pub fn new(model: Arc<dyn StructuredModel>) -> Self {
        Self { model }
    }
}

impl CodeReviewer for ClaudeReviewer {
    async fn review(&self, input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
        let system = format!("{REVIEW_SYSTEM_PROMPT}\n\n{}", guidelines_block(input.guidelines));
        let user = review_user_message(input.files, input.callers);
        let answer =
            self.model.structured::<ReviewAnswer>(&system, &user, &review_schema()).await.map_err(reviewer_error)?;
        let (usage, cost_usd) = spent(&answer);
        Ok(ReviewerOutput { findings: answer.value.into_findings(), usage, cost_usd })
    }

    async fn assess_requirements(&self, input: RequirementsInput<'_>) -> Result<AssessmentOutput, ReviewerError> {
        let user = requirements_user_message(input.requirements, input.files);
        let answer = self
            .model
            .structured::<AssessmentAnswer>(REQUIREMENTS_SYSTEM_PROMPT, &user, &assessment_schema())
            .await
            .map_err(reviewer_error)?;
        let (usage, cost_usd) = spent(&answer);
        Ok(AssessmentOutput { assessments: answer.value.into_assessments(), usage, cost_usd })
    }
}

pub(super) fn spent<T>(answer: &Structured<T>) -> (TokenUsage, Option<f64>) {
    let Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens } = answer.usage;
    (TokenUsage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens }, Some(answer.cost_usd))
}

pub(super) fn reviewer_error(error: LlmError) -> ReviewerError {
    match error {
        LlmError::Truncated => ReviewerError::Truncated,
        LlmError::Refused { category } => ReviewerError::Refused { category },
        LlmError::InvalidResponse(reason) => ReviewerError::InvalidResponse(reason),
        LlmError::Rejected(reason) | LlmError::Unsupported(reason) => ReviewerError::Rejected(reason),
        limited @ LlmError::RateLimited { .. } => ReviewerError::RateLimited(limited.to_string()),
        other @ LlmError::Unavailable(_) => ReviewerError::Unavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::llm::FakeModel;
    use crate::review::domain::change_set::FileChange;
    use crate::review::domain::change_set::fixtures::{added, file};
    use crate::review::domain::finding::{Category, Severity};
    use crate::review::domain::requirements::{AcceptanceCriterion, CriterionStatus, Requirements};

    fn files() -> Vec<FileChange> {
        vec![file("app.py", 1, vec![added(1, "eval(user_input)")], Some("eval(user_input)\n"))]
    }

    fn finding_json() -> Value {
        json!({"findings": [{
            "file": "app.py", "line": 1, "severity": "critical", "category": "security", "confidence": "high",
            "title": "eval on user input", "explanation": "RCE", "evidence": "eval(user_input)",
            "suggestion": "use ast.literal_eval",
            "prevention_rule": "Never pass user input to eval.", "rule_scope": "**/*.py"
        }]})
    }

    fn requirements() -> Requirements {
        Requirements {
            key: "SHOP-1".into(),
            title: "t".into(),
            url: None,
            criteria: vec![AcceptanceCriterion { id: "SHOP-1/AC-1".into(), text: "evaluates input".into() }],
        }
    }

    async fn review_with(fake: &Arc<FakeModel>) -> Result<ReviewerOutput, ReviewerError> {
        let files = files();
        ClaudeReviewer::new(fake.clone())
            .review(ReviewInput { files: &files, guidelines: "Prefer explicit errors.", callers: &[] })
            .await
    }

    #[tokio::test]
    async fn review_parses_findings_and_reports_usage_and_cost() {
        let fake = Arc::new(FakeModel::answering(finding_json()));

        let output = review_with(&fake).await.unwrap();

        assert_eq!(output.findings.len(), 1);
        let finding = &output.findings[0];
        assert_eq!((finding.severity, finding.category), (Severity::Critical, Category::Security));
        assert_eq!(finding.suggestion.as_deref(), Some("use ast.literal_eval"));
        assert_eq!((output.usage.input_tokens, output.usage.output_tokens), (100, 10));
        assert_eq!(output.cost_usd, Some(0.5));
    }

    #[tokio::test]
    async fn review_sends_instructions_guidelines_diff_and_schema() {
        let fake = Arc::new(FakeModel::answering(json!({"findings": []})));

        review_with(&fake).await.unwrap();

        let calls = fake.calls.lock().unwrap();
        let (system, user, schema) = &calls[0];
        assert!(system.starts_with(REVIEW_SYSTEM_PROMPT));
        assert!(system.ends_with("</repository_guidelines>") && system.contains("Prefer explicit errors."));
        assert!(user.contains("1 + eval(user_input)"));
        assert_eq!(schema, &review_schema());
    }

    #[tokio::test]
    async fn assess_requirements_parses_assessments() {
        let fake = Arc::new(FakeModel::answering(json!({"assessments": [
            {"criterion_id": "SHOP-1/AC-1", "status": "met", "explanation": "ok",
             "evidence": [{"file": "app.py", "line": 1, "quote": "eval(user_input)"}]}
        ]})));
        let files = files();
        let requirements = requirements();

        let output = ClaudeReviewer::new(fake.clone())
            .assess_requirements(RequirementsInput { files: &files, requirements: &requirements })
            .await
            .unwrap();

        assert_eq!(output.assessments.len(), 1);
        assert_eq!(output.assessments[0].status, CriterionStatus::Met);
        assert_eq!(output.cost_usd, Some(0.5));
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, REQUIREMENTS_SYSTEM_PROMPT);
        assert!(calls[0].1.contains("SHOP-1/AC-1"));
        assert_eq!(calls[0].2, assessment_schema());
    }

    #[tokio::test]
    async fn llm_errors_map_to_reviewer_errors() {
        let cases = [
            (LlmError::Truncated, "truncated"),
            (LlmError::Refused { category: Some("cyber".into()) }, "refused"),
            (LlmError::InvalidResponse("x".into()), "invalid"),
            (LlmError::Rejected("x".into()), "rejected"),
            (LlmError::Unavailable("x".into()), "unavailable"),
            (LlmError::RateLimited { resets_at: Some("5pm".into()), message: "m".into() }, "rate_limited"),
        ];
        for (error, expected) in cases {
            let fake = Arc::new(FakeModel::new(vec![Err(error)]));
            let result = review_with(&fake).await.unwrap_err();
            let actual = match &result {
                ReviewerError::Truncated => "truncated",
                ReviewerError::Refused { category } => {
                    assert_eq!(category.as_deref(), Some("cyber"));
                    "refused"
                }
                ReviewerError::InvalidResponse(_) => "invalid",
                ReviewerError::Rejected(_) => "rejected",
                ReviewerError::RateLimited(m) => {
                    assert!(m.contains("5pm"));
                    "rate_limited"
                }
                ReviewerError::Unavailable(m) => {
                    assert!(!m.is_empty());
                    "unavailable"
                }
            };
            assert_eq!(actual, expected);
        }
    }

    #[tokio::test]
    async fn rate_limit_message_keeps_the_reset_time() {
        let limited = LlmError::RateLimited { resets_at: Some("5pm".into()), message: "limit".into() };
        let fake = Arc::new(FakeModel::new(vec![Err(limited)]));
        let error = review_with(&fake).await.unwrap_err();
        assert!(error.to_string().contains("resets 5pm"), "{error}");
    }
}
