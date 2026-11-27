use super::ports::{CodeReviewer, RequirementsInput, ReviewerError};
use crate::review::domain::change_set::ChangeSet;
use crate::review::domain::report::TokenUsage;
use crate::review::domain::requirements::{
    CriterionAssessment, CriterionStatus, Requirements, RequirementsReport, verify_assessments,
};

pub const MAX_RETRY_CRITERIA: usize = 10;

pub struct CriteriaRetry {
    pub report: RequirementsReport,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
    pub note: Option<String>,
}

impl CriteriaRetry {
    fn unchanged(report: RequirementsReport, note: Option<String>) -> Self {
        Self { report, usage: TokenUsage::default(), cost_usd: None, note }
    }
}

fn is_retryable(assessment: &CriterionAssessment) -> bool {
    assessment.status == CriterionStatus::Unclear && assessment.reason.is_some_and(|r| r.is_retryable())
}

fn retry_subset(report: &RequirementsReport) -> Requirements {
    let ids: Vec<&str> = report
        .assessments
        .iter()
        .filter(|a| is_retryable(a))
        .take(MAX_RETRY_CRITERIA)
        .map(|a| a.criterion_id.as_str())
        .collect();
    let criteria = report.requirements.criteria.iter().filter(|c| ids.contains(&c.id.as_str())).cloned().collect();
    Requirements { criteria, ..report.requirements.clone() }
}

pub async fn retry_unverified<R: CodeReviewer>(
    reviewer: &R,
    changes: &ChangeSet,
    report: RequirementsReport,
    clarifications: &[(String, String)],
) -> Result<CriteriaRetry, ReviewerError> {
    let subset = retry_subset(&report);
    if subset.criteria.is_empty() {
        return Ok(CriteriaRetry::unchanged(report, None));
    }
    let relevant: Vec<(String, String)> =
        clarifications.iter().filter(|(id, _)| subset.criteria.iter().any(|c| &c.id == id)).cloned().collect();
    let input =
        RequirementsInput { files: &changes.files, requirements: &subset, clarifications: &relevant, expanded: true };
    let count = subset.criteria.len();
    let output = match reviewer.assess_requirements(input).await {
        Ok(output) => output,
        Err(error) if error.is_fatal() => return Err(error),
        Err(error) => {
            let note = format!("retry of {count} unverifiable criteria with full-file context failed: {error}");
            return Ok(CriteriaRetry::unchanged(report, Some(note)));
        }
    };
    let rechecked = verify_assessments(changes, subset, output.assessments).assessments;
    let resolved = rechecked.iter().filter(|a| a.status != CriterionStatus::Unclear).count();
    let note = format!("retried {count} unverifiable criteria with full-file context; {resolved} resolved");
    Ok(CriteriaRetry {
        report: merge_retried(report, rechecked),
        usage: output.usage,
        cost_usd: output.cost_usd,
        note: Some(note),
    })
}

fn merge_retried(mut report: RequirementsReport, rechecked: Vec<CriterionAssessment>) -> RequirementsReport {
    for fresh in rechecked {
        if let Some(slot) = report.assessments.iter_mut().find(|a| a.criterion_id == fresh.criterion_id) {
            *slot = CriterionAssessment { retried: true, ..fresh };
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::review::application::ports::{AssessmentOutput, ReviewInput, ReviewerOutput};
    use crate::review::domain::change_set::fixtures::{added, file};
    use crate::review::domain::requirements::{AcceptanceCriterion, EvidenceRef};
    use crate::review::domain::unverifiable::UnverifiableReason;

    type AssessCall = (Vec<String>, bool, Vec<String>);

    struct Scripted {
        answer: Mutex<Option<Result<AssessmentOutput, ReviewerError>>>,
        seen: Mutex<Vec<AssessCall>>,
    }

    impl Scripted {
        fn new(answer: Result<AssessmentOutput, ReviewerError>) -> Self {
            Self { answer: Mutex::new(Some(answer)), seen: Mutex::new(Vec::new()) }
        }
    }

    impl CodeReviewer for Scripted {
        async fn review(&self, _input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
            Ok(ReviewerOutput::default())
        }

        async fn assess_requirements(&self, input: RequirementsInput<'_>) -> Result<AssessmentOutput, ReviewerError> {
            let ids = input.requirements.criteria.iter().map(|c| c.id.clone()).collect();
            let notes = input.clarifications.iter().map(|(id, _)| id.clone()).collect();
            self.seen.lock().unwrap().push((ids, input.expanded, notes));
            self.answer.lock().unwrap().take().expect("one call")
        }
    }

    fn changes() -> ChangeSet {
        ChangeSet { files: vec![file("a.go", 1, vec![added(1, "total := compute(1)")], Some("total := compute(1)\n"))] }
    }

    fn report(reasons: &[(&str, CriterionStatus, Option<UnverifiableReason>)]) -> RequirementsReport {
        let criteria = reasons.iter().map(|(id, _, _)| AcceptanceCriterion::new(*id, "t")).collect();
        let assessments = reasons
            .iter()
            .map(|(id, status, reason)| CriterionAssessment {
                criterion_id: (*id).into(),
                status: *status,
                explanation: "first".into(),
                evidence: Vec::new(),
                reason: *reason,
                retried: false,
                human: None,
            })
            .collect();
        RequirementsReport {
            requirements: Requirements { key: "K-1".into(), title: "t".into(), url: None, criteria },
            assessments,
        }
    }

    fn met(id: &str) -> CriterionAssessment {
        CriterionAssessment {
            criterion_id: id.into(),
            status: CriterionStatus::Met,
            explanation: "found it".into(),
            evidence: vec![EvidenceRef { file: "a.go".into(), line: 1, quote: "total := compute(1)".into() }],
            reason: None,
            retried: false,
            human: None,
        }
    }

    fn mixed() -> RequirementsReport {
        report(&[
            ("AC-1", CriterionStatus::Unclear, Some(UnverifiableReason::NeedsContext)),
            ("AC-2", CriterionStatus::Unclear, Some(UnverifiableReason::AmbiguousRequirement)),
            ("AC-3", CriterionStatus::Met, None),
            ("AC-4", CriterionStatus::Unclear, Some(UnverifiableReason::NoEvidence)),
        ])
    }

    #[tokio::test]
    async fn retries_only_retryable_unclear_criteria_and_marks_them_retried() {
        let reviewer = Scripted::new(Ok(AssessmentOutput {
            assessments: vec![met("AC-1")],
            usage: TokenUsage { input_tokens: 5, ..TokenUsage::default() },
            cost_usd: Some(0.2),
        }));
        let clarifications = vec![("AC-1".to_string(), "x".to_string()), ("AC-2".to_string(), "y".to_string())];

        let retry = retry_unverified(&reviewer, &changes(), mixed(), &clarifications).await.unwrap();

        assert_eq!(
            *reviewer.seen.lock().unwrap(),
            vec![(vec!["AC-1".into(), "AC-4".into()], true, vec!["AC-1".into()])]
        );
        let statuses: Vec<_> = retry.report.assessments.iter().map(|a| (a.status, a.retried)).collect();
        assert_eq!(
            statuses,
            vec![
                (CriterionStatus::Met, true),
                (CriterionStatus::Unclear, false),
                (CriterionStatus::Met, false),
                (CriterionStatus::Unclear, true),
            ]
        );
        assert_eq!(retry.report.assessments[3].reason, Some(UnverifiableReason::NotAssessed));
        assert_eq!(retry.usage.input_tokens, 5);
        assert_eq!(retry.cost_usd, Some(0.2));
        assert_eq!(retry.note.as_deref(), Some("retried 2 unverifiable criteria with full-file context; 1 resolved"));
    }

    #[tokio::test]
    async fn nothing_retryable_makes_no_call() {
        let reviewer = Scripted::new(Err(ReviewerError::Truncated));
        let only_ambiguous =
            report(&[("AC-1", CriterionStatus::Unclear, Some(UnverifiableReason::AmbiguousRequirement))]);

        let retry = retry_unverified(&reviewer, &changes(), only_ambiguous.clone(), &[]).await.unwrap();

        assert!(reviewer.seen.lock().unwrap().is_empty());
        assert_eq!(retry.report, only_ambiguous);
        assert_eq!(retry.note, None);
    }

    #[tokio::test]
    async fn non_fatal_failure_keeps_the_originals_and_notes_it() {
        let reviewer = Scripted::new(Err(ReviewerError::Unavailable("down".into())));

        let retry = retry_unverified(&reviewer, &changes(), mixed(), &[]).await.unwrap();

        assert_eq!(retry.report, mixed());
        assert!(retry.note.unwrap().contains("failed"));
        assert_eq!(retry.usage, TokenUsage::default());
    }

    #[tokio::test]
    async fn fatal_failure_propagates() {
        let reviewer = Scripted::new(Err(ReviewerError::RateLimited("limit".into())));
        let error = retry_unverified(&reviewer, &changes(), mixed(), &[]).await.err();
        assert_eq!(error, Some(ReviewerError::RateLimited("limit".into())));
    }

    #[tokio::test]
    async fn retries_at_most_the_configured_number_of_criteria() {
        let many: Vec<_> = (0..MAX_RETRY_CRITERIA + 3).map(|i| format!("AC-{i}")).collect();
        let rows: Vec<_> = many
            .iter()
            .map(|id| (id.as_str(), CriterionStatus::Unclear, Some(UnverifiableReason::ToolError)))
            .collect();
        let reviewer = Scripted::new(Ok(AssessmentOutput::default()));

        retry_unverified(&reviewer, &changes(), report(&rows), &[]).await.unwrap();

        assert_eq!(reviewer.seen.lock().unwrap()[0].0.len(), MAX_RETRY_CRITERIA);
    }
}
