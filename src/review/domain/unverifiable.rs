use serde::{Deserialize, Serialize};

use super::agent_results::{AgentReport, TestVerdict};
use super::report::{SkipReason, SkippedFile};
use super::requirements::{CriterionStatus, HumanMark, RequirementsReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnverifiableReason {
    AmbiguousRequirement,
    NeedsContext,
    NoEvidence,
    NotAssessed,
    ToolError,
    TestNotRunnable,
    TestTimeout,
    TestUnrepairable,
    TestEvidenceMissing,
    ReviewFailed,
}

impl UnverifiableReason {
    pub const ALL: [Self; 10] = [
        Self::AmbiguousRequirement,
        Self::NeedsContext,
        Self::NoEvidence,
        Self::NotAssessed,
        Self::ToolError,
        Self::TestNotRunnable,
        Self::TestTimeout,
        Self::TestUnrepairable,
        Self::TestEvidenceMissing,
        Self::ReviewFailed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AmbiguousRequirement => "ambiguous_requirement",
            Self::NeedsContext => "needs_context",
            Self::NoEvidence => "no_evidence",
            Self::NotAssessed => "not_assessed",
            Self::ToolError => "tool_error",
            Self::TestNotRunnable => "test_not_runnable",
            Self::TestTimeout => "test_timeout",
            Self::TestUnrepairable => "test_unrepairable",
            Self::TestEvidenceMissing => "test_evidence_missing",
            Self::ReviewFailed => "review_failed",
        }
    }

    pub fn is_retryable(self) -> bool {
        matches!(self, Self::NeedsContext | Self::NoEvidence | Self::NotAssessed | Self::ToolError | Self::ReviewFailed)
    }

    pub fn remedy(self) -> &'static str {
        match self {
            Self::AmbiguousRequirement => {
                "Clarify the acceptance criterion in the ticket, or answer the clarification on the dashboard"
            }
            Self::NeedsContext => "Add the missing context to the ticket, or verify the criterion manually",
            Self::NoEvidence => "Point the change at concrete code for this criterion, or verify it manually",
            Self::NotAssessed => "Re-run the review, or verify the criterion manually",
            Self::ToolError => "Re-run the review once the model API is healthy",
            Self::TestNotRunnable => "Install the toolchain or fix the test setup (QGATE_TOOLCHAINS)",
            Self::TestTimeout => "Raise the test timeout or simplify the scenario, then re-run the review",
            Self::TestUnrepairable => "Make the code under test easier to exercise, or verify the scenario manually",
            Self::TestEvidenceMissing => "Re-run the review to regenerate the failing test with complete evidence",
            Self::ReviewFailed => "Re-run the review for this file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnverifiableSubject {
    Criterion,
    Test,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unverifiable {
    pub subject: UnverifiableSubject,
    pub id: String,
    pub reason: UnverifiableReason,
    pub detail: String,
    #[serde(default)]
    pub retried: bool,
    pub remedy: String,
}

impl Unverifiable {
    fn new(subject: UnverifiableSubject, id: &str, reason: UnverifiableReason, detail: &str, retried: bool) -> Self {
        Self {
            subject,
            id: id.to_string(),
            reason,
            detail: detail.to_string(),
            retried,
            remedy: reason.remedy().to_string(),
        }
    }
}

pub fn collect_unverifiable(
    requirements: Option<&RequirementsReport>,
    agents: Option<&AgentReport>,
    skipped: &[SkippedFile],
) -> Vec<Unverifiable> {
    let criteria = requirements.into_iter().flat_map(|r| &r.assessments).filter_map(|a| {
        if a.status != CriterionStatus::Unclear || a.human == Some(HumanMark::NotApplicable) {
            return None;
        }
        let reason = a.reason.unwrap_or(UnverifiableReason::AmbiguousRequirement);
        Some(Unverifiable::new(UnverifiableSubject::Criterion, &a.criterion_id, reason, &a.explanation, a.retried))
    });
    let tests = agents.into_iter().flat_map(|a| &a.tests).filter(|t| t.verdict == TestVerdict::Inconclusive).map(|t| {
        let reason = t.reason.unwrap_or(UnverifiableReason::TestUnrepairable);
        Unverifiable::new(UnverifiableSubject::Test, &t.scenario_id, reason, &t.note, false)
    });
    let files = skipped.iter().filter_map(|s| match &s.reason {
        SkipReason::ReviewFailed(message) => Some(Unverifiable::new(
            UnverifiableSubject::File,
            &s.path,
            UnverifiableReason::ReviewFailed,
            message,
            false,
        )),
        _ => None,
    });
    criteria.chain(tests).chain(files).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::agent_results::TestResult;
    use crate::review::domain::requirements::{AcceptanceCriterion, CriterionAssessment, Requirements};

    fn assessment(id: &str, status: CriterionStatus, reason: Option<UnverifiableReason>) -> CriterionAssessment {
        CriterionAssessment {
            criterion_id: id.into(),
            status,
            explanation: format!("{id} detail"),
            evidence: Vec::new(),
            reason,
            retried: false,
            human: None,
        }
    }

    fn report(assessments: Vec<CriterionAssessment>) -> RequirementsReport {
        let criteria = assessments.iter().map(|a| AcceptanceCriterion::new(a.criterion_id.clone(), "t")).collect();
        RequirementsReport {
            requirements: Requirements { key: "K-1".into(), title: "t".into(), url: None, criteria },
            assessments,
        }
    }

    fn test(id: &str, verdict: TestVerdict, reason: Option<UnverifiableReason>) -> TestResult {
        TestResult {
            scenario_id: id.into(),
            language: "go".into(),
            verdict,
            evidence: None,
            note: format!("{id} note"),
            reason,
        }
    }

    #[test]
    fn names_match_serde_and_every_reason_has_a_remedy() {
        for reason in UnverifiableReason::ALL {
            assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
            assert!(!reason.remedy().is_empty());
        }
    }

    #[test]
    fn only_transient_reasons_are_retryable() {
        let retryable: Vec<_> = UnverifiableReason::ALL.into_iter().filter(|r| r.is_retryable()).collect();
        assert_eq!(
            retryable,
            vec![
                UnverifiableReason::NeedsContext,
                UnverifiableReason::NoEvidence,
                UnverifiableReason::NotAssessed,
                UnverifiableReason::ToolError,
                UnverifiableReason::ReviewFailed,
            ]
        );
    }

    #[test]
    fn collects_unclear_criteria_with_reason_fallback_and_retried_flag() {
        let mut retried = assessment("AC-2", CriterionStatus::Unclear, Some(UnverifiableReason::NoEvidence));
        retried.retried = true;
        let requirements = report(vec![
            assessment("AC-1", CriterionStatus::Unclear, None),
            retried,
            assessment("AC-3", CriterionStatus::Met, None),
            assessment("AC-4", CriterionStatus::NotMet, None),
        ]);
        let found = collect_unverifiable(Some(&requirements), None, &[]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].reason, UnverifiableReason::AmbiguousRequirement);
        assert_eq!(found[0].subject, UnverifiableSubject::Criterion);
        assert_eq!(found[0].remedy, UnverifiableReason::AmbiguousRequirement.remedy());
        assert_eq!(found[1].reason, UnverifiableReason::NoEvidence);
        assert!(found[1].retried);
        assert_eq!(found[1].detail, "AC-2 detail");
    }

    #[test]
    fn human_not_applicable_criteria_are_excluded() {
        let mut na = assessment("AC-1", CriterionStatus::Unclear, None);
        na.human = Some(HumanMark::NotApplicable);
        assert!(collect_unverifiable(Some(&report(vec![na])), None, &[]).is_empty());
    }

    #[test]
    fn collects_inconclusive_tests_with_fallback_reason() {
        let agents = AgentReport {
            tests: vec![
                test("S1", TestVerdict::Passed, None),
                test("S2", TestVerdict::Inconclusive, Some(UnverifiableReason::TestTimeout)),
                test("S3", TestVerdict::Inconclusive, None),
            ],
            ..AgentReport::default()
        };
        let found = collect_unverifiable(None, Some(&agents), &[]);
        let summary: Vec<_> = found.iter().map(|u| (u.id.as_str(), u.reason)).collect();
        assert_eq!(
            summary,
            vec![("S2", UnverifiableReason::TestTimeout), ("S3", UnverifiableReason::TestUnrepairable)]
        );
        assert!(found.iter().all(|u| u.subject == UnverifiableSubject::Test && !u.retried));
    }

    #[test]
    fn collects_only_failed_file_reviews() {
        let skipped = vec![
            SkippedFile { path: "a.go".into(), reason: SkipReason::Binary },
            SkippedFile { path: "b.go".into(), reason: SkipReason::ReviewFailed("boom".into()) },
        ];
        let found = collect_unverifiable(None, None, &skipped);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subject, UnverifiableSubject::File);
        assert_eq!(found[0].id, "b.go");
        assert_eq!(found[0].detail, "boom");
        assert_eq!(found[0].reason, UnverifiableReason::ReviewFailed);
    }

    #[test]
    fn nothing_to_collect_yields_empty() {
        assert!(collect_unverifiable(None, None, &[]).is_empty());
    }
}
