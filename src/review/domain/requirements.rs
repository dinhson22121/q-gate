use serde::{Deserialize, Serialize};

use super::change_set::ChangeSet;
use super::verification::evidence_exists;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub text: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriterionAssessment {
    pub criterion_id: String,
    pub status: CriterionStatus,
    pub explanation: String,
    pub evidence: Vec<EvidenceRef>,
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
        self.assessments.iter().filter(|a| a.status != CriterionStatus::Met).map(|a| a.criterion_id.as_str()).collect()
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
                .unwrap_or_else(|| CriterionAssessment {
                    criterion_id: criterion.id.clone(),
                    status: CriterionStatus::Unclear,
                    explanation: "Not assessed by the reviewer.".into(),
                    evidence: Vec::new(),
                })
        })
        .collect();
    RequirementsReport { requirements, assessments: verified }
}

fn verify_one(changes: &ChangeSet, mut assessment: CriterionAssessment) -> CriterionAssessment {
    assessment.evidence.retain(|e| evidence_exists(changes, &e.file, e.line, &e.quote));
    if assessment.status == CriterionStatus::Met && assessment.evidence.is_empty() {
        assessment.status = CriterionStatus::Unclear;
        assessment.explanation =
            format!("{} (downgraded: claimed met but no verifiable evidence in the change)", assessment.explanation);
    }
    assessment
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
                AcceptanceCriterion { id: "AC-1".into(), text: "User can export CSV".into() },
                AcceptanceCriterion { id: "AC-2".into(), text: "Export is audited".into() },
                AcceptanceCriterion { id: "AC-3".into(), text: "Works offline".into() },
            ],
        }
    }

    fn assessment(id: &str, status: CriterionStatus, quote: &str) -> CriterionAssessment {
        CriterionAssessment {
            criterion_id: id.into(),
            status,
            explanation: "why".into(),
            evidence: vec![EvidenceRef { file: "api.py".into(), line: 1, quote: quote.into() }],
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
        assert!(report.assessments[0].evidence.is_empty());
        assert_eq!(report.assessments.len(), 3);
    }

    #[test]
    fn status_names_match_serde() {
        for s in CriterionStatus::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
    }
}
