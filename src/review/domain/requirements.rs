use serde::{Deserialize, Serialize};

use super::change_set::ChangeSet;
use super::unverifiable::UnverifiableReason;
use super::verification::evidence_exists;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inferred: bool,
}

impl AcceptanceCriterion {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self { id: id.into(), text: text.into(), inferred: false }
    }

    pub fn inferred(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self { inferred: true, ..Self::new(id, text) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Requirements {
    pub key: String,
    pub title: String,
    pub url: Option<String>,
    pub criteria: Vec<AcceptanceCriterion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    Met,
    NotMet,
    Unclear,
}

impl CriterionStatus {
    pub const ALL: [Self; 3] = [Self::Met, Self::NotMet, Self::Unclear];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::NotMet => "not_met",
            Self::Unclear => "unclear",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub file: String,
    pub line: u32,
    pub quote: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanMark {
    VerifiedMet,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriterionAssessment {
    pub criterion_id: String,
    pub status: CriterionStatus,
    pub explanation: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnverifiableReason>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retried: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human: Option<HumanMark>,
}

impl CriterionAssessment {
    pub fn unclear(criterion_id: impl Into<String>, reason: UnverifiableReason, explanation: String) -> Self {
        Self {
            criterion_id: criterion_id.into(),
            status: CriterionStatus::Unclear,
            explanation,
            evidence: Vec::new(),
            reason: Some(reason),
            retried: false,
            human: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequirementsReport {
    pub requirements: Requirements,
    pub assessments: Vec<CriterionAssessment>,
}

impl RequirementsReport {
    pub fn unmet(&self) -> Vec<&str> {
        self.assessments
            .iter()
            .filter(|a| a.status == CriterionStatus::NotMet)
            .map(|a| a.criterion_id.as_str())
            .collect()
    }

    pub fn unsatisfied(&self) -> Vec<&str> {
        self.assessments
            .iter()
            .filter(|a| a.status != CriterionStatus::Met && a.human != Some(HumanMark::NotApplicable))
            .map(|a| a.criterion_id.as_str())
            .collect()
    }

    pub fn unverified(&self) -> Vec<&str> {
        self.assessments
            .iter()
            .filter(|a| a.status == CriterionStatus::Unclear && a.human != Some(HumanMark::NotApplicable))
            .map(|a| a.criterion_id.as_str())
            .collect()
    }

    pub fn is_human(&self, criterion_id: &str) -> Option<HumanMark> {
        self.assessments.iter().find(|a| a.criterion_id == criterion_id).and_then(|a| a.human)
    }

    pub fn is_inferred(&self, criterion_id: &str) -> bool {
        self.requirements.criteria.iter().any(|c| c.id == criterion_id && c.inferred)
    }

    pub fn met_count(&self) -> usize {
        self.assessments.iter().filter(|a| a.status == CriterionStatus::Met).count()
    }
}

pub fn verify_assessments(
    changes: &ChangeSet,
    requirements: Requirements,
    assessments: Vec<CriterionAssessment>,
) -> RequirementsReport {
    let verified = requirements
        .criteria
        .iter()
        .map(|criterion| {
            assessments
                .iter()
                .find(|a| a.criterion_id == criterion.id)
                .cloned()
                .map(|a| verify_one(changes, a))
                .unwrap_or_else(|| {
                    CriterionAssessment::unclear(
                        criterion.id.clone(),
                        UnverifiableReason::NotAssessed,
                        "Not assessed by the reviewer.".into(),
                    )
                })
        })
        .collect();
    RequirementsReport { requirements, assessments: verified }
}

fn verify_one(changes: &ChangeSet, mut assessment: CriterionAssessment) -> CriterionAssessment {
    assessment.evidence.retain(|e| evidence_exists(changes, &e.file, e.line, &e.quote));
    if assessment.status == CriterionStatus::Met && assessment.evidence.is_empty() {
        assessment.status = CriterionStatus::Unclear;
        assessment.reason = Some(UnverifiableReason::NoEvidence);
        assessment.explanation =
            format!("{} (downgraded: claimed met but no verifiable evidence in the change)", assessment.explanation);
    }
    assessment
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HumanInput {
    pub mark: Option<HumanMark>,
    pub clarification: Option<String>,
}

pub const VERIFIED_NOTE: &str = " (verified manually by a person)";
pub const STALE_MARK_NOTE: &str = " (a person had marked this verified, but the reviewer now finds it not met)";

pub fn apply_human_marks(mut report: RequirementsReport, inputs: &[HumanInput]) -> RequirementsReport {
    let criteria = report.requirements.criteria.clone();
    for (criterion, input) in criteria.iter().zip(inputs) {
        let Some(mark) = input.mark else { continue };
        let Some(assessment) = report.assessments.iter_mut().find(|a| a.criterion_id == criterion.id) else {
            continue;
        };
        apply_mark(assessment, mark);
    }
    report
}

fn apply_mark(assessment: &mut CriterionAssessment, mark: HumanMark) {
    match (assessment.status, mark) {
        (CriterionStatus::Unclear, HumanMark::VerifiedMet) => {
            assessment.status = CriterionStatus::Met;
            assessment.reason = None;
            assessment.human = Some(mark);
            assessment.explanation.push_str(VERIFIED_NOTE);
        }
        (CriterionStatus::Unclear, HumanMark::NotApplicable) => assessment.human = Some(mark),
        (CriterionStatus::NotMet, _) => assessment.explanation.push_str(STALE_MARK_NOTE),
        (CriterionStatus::Met, _) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, file};

    fn changes() -> ChangeSet {
        ChangeSet {
            files: vec![file("api.py", 1, vec![added(1, "def export_csv(rows):")], Some("def export_csv(rows):\n"))],
        }
    }

    fn requirements() -> Requirements {
        Requirements {
            key: "ABC-1".into(),
            title: "Export".into(),
            url: None,
            criteria: vec![
                AcceptanceCriterion::new("AC-1", "User can export CSV"),
                AcceptanceCriterion::new("AC-2", "Export is audited"),
                AcceptanceCriterion::new("AC-3", "Works offline"),
            ],
        }
    }

    fn assessment(id: &str, status: CriterionStatus, quote: &str) -> CriterionAssessment {
        CriterionAssessment {
            criterion_id: id.into(),
            status,
            explanation: "why".into(),
            evidence: vec![EvidenceRef { file: "api.py".into(), line: 1, quote: quote.into() }],
            reason: None,
            retried: false,
            human: None,
        }
    }

    #[test]
    fn met_with_real_evidence_stays_met_and_not_met_is_reported() {
        let report = verify_assessments(
            &changes(),
            requirements(),
            vec![
                assessment("AC-1", CriterionStatus::Met, "def export_csv(rows):"),
                assessment("AC-2", CriterionStatus::NotMet, "nothing"),
            ],
        );
        let statuses: Vec<_> = report.assessments.iter().map(|a| a.status).collect();
        assert_eq!(statuses, vec![CriterionStatus::Met, CriterionStatus::NotMet, CriterionStatus::Unclear]);
        assert_eq!(report.unmet(), vec!["AC-2"]);
    }

    #[test]
    fn met_without_verifiable_evidence_is_downgraded() {
        let report = verify_assessments(
            &changes(),
            requirements(),
            vec![
                assessment("AC-1", CriterionStatus::Met, "audit_log.write()"),
                assessment("AC-9", CriterionStatus::Met, "x"),
            ],
        );
        assert_eq!(report.assessments[0].status, CriterionStatus::Unclear);
        assert_eq!(report.assessments[0].reason, Some(UnverifiableReason::NoEvidence));
        assert!(report.assessments[0].evidence.is_empty());
        assert_eq!(report.assessments.len(), 3);
        assert_eq!(report.assessments[2].reason, Some(UnverifiableReason::NotAssessed));
    }

    fn marked(mark: Option<HumanMark>) -> HumanInput {
        HumanInput { mark, clarification: None }
    }

    fn report_with(statuses: [CriterionStatus; 3]) -> RequirementsReport {
        let assessments = ["AC-1", "AC-2", "AC-3"]
            .into_iter()
            .zip(statuses)
            .map(|(id, status)| match status {
                CriterionStatus::Unclear => {
                    CriterionAssessment::unclear(id, UnverifiableReason::NeedsContext, "unsure".into())
                }
                status => assessment(id, status, "def export_csv(rows):"),
            })
            .collect();
        RequirementsReport { requirements: requirements(), assessments }
    }

    #[test]
    fn verified_mark_turns_unclear_into_met() {
        let report = report_with([CriterionStatus::Unclear, CriterionStatus::Met, CriterionStatus::Met]);
        let applied = apply_human_marks(report, &[marked(Some(HumanMark::VerifiedMet)), marked(None), marked(None)]);
        assert_eq!(applied.assessments[0].status, CriterionStatus::Met);
        assert_eq!(applied.assessments[0].human, Some(HumanMark::VerifiedMet));
        assert_eq!(applied.assessments[0].reason, None);
        assert!(applied.assessments[0].explanation.ends_with(VERIFIED_NOTE));
        assert!(applied.unsatisfied().is_empty());
    }

    #[test]
    fn not_applicable_mark_keeps_unclear_but_stops_it_counting() {
        let report = report_with([CriterionStatus::Unclear, CriterionStatus::Met, CriterionStatus::Met]);
        let applied = apply_human_marks(report, &[marked(Some(HumanMark::NotApplicable))]);
        assert_eq!(applied.assessments[0].status, CriterionStatus::Unclear);
        assert_eq!(applied.is_human("AC-1"), Some(HumanMark::NotApplicable));
        assert!(applied.unsatisfied().is_empty());
        assert!(applied.unverified().is_empty());
    }

    #[test]
    fn marks_never_override_not_met_or_met() {
        let report = report_with([CriterionStatus::NotMet, CriterionStatus::Met, CriterionStatus::Unclear]);
        let inputs = [marked(Some(HumanMark::VerifiedMet)), marked(Some(HumanMark::VerifiedMet)), marked(None)];
        let applied = apply_human_marks(report, &inputs);
        assert_eq!(applied.assessments[0].status, CriterionStatus::NotMet);
        assert!(applied.assessments[0].explanation.ends_with(STALE_MARK_NOTE));
        assert_eq!(applied.assessments[0].human, None);
        assert_eq!(applied.assessments[1].explanation, "why");
        assert_eq!(applied.assessments[1].human, None);
        assert_eq!(applied.unverified(), vec!["AC-3"]);
        assert_eq!(applied.is_human("missing"), None);
    }

    #[test]
    fn status_names_match_serde() {
        for s in CriterionStatus::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
    }
}
