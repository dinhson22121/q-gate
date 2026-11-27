use serde::{Deserialize, Serialize};

use super::agent_view::{AgentReportView, TicketContextView};
use crate::review::application::{
    Category, Confidence, CriterionStatus, HumanMark, Severity, Unverifiable, UnverifiableReason,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportView {
    #[serde(default)]
    pub files_reviewed: Vec<String>,
    #[serde(default)]
    pub skipped: Vec<SkippedView>,
    #[serde(default)]
    pub findings: Vec<FindingView>,
    #[serde(default)]
    pub rejected: Vec<RejectedView>,
    #[serde(default)]
    pub requirements: Option<RequirementsView>,
    #[serde(default)]
    pub checks: Vec<CheckView>,
    pub gate: GateView,
    #[serde(default)]
    pub usage: UsageView,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub agents: Option<AgentReportView>,
    #[serde(default)]
    pub ticket: Option<TicketContextView>,
    #[serde(default)]
    pub audited: bool,
    #[serde(default)]
    pub unverifiable: Vec<Unverifiable>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedView {
    pub path: String,
    pub reason: SkipReasonView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipReasonView {
    pub kind: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingView {
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub category: Category,
    pub confidence: Confidence,
    pub title: String,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub prevention_rule: Option<PreventionRuleView>,
    #[serde(default)]
    pub source: Option<FindingSourceView>,
    #[serde(default)]
    pub reproduce: Option<ReproduceView>,
    #[serde(default, deserialize_with = "demotion_name")]
    pub demoted: Option<String>,
    #[serde(
        default,
        rename(serialize = "dismissed", deserialize = "feedback"),
        alias = "dismissed",
        deserialize_with = "dismissed_flag"
    )]
    pub dismissed: bool,
}

fn demotion_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(name)) => Some(name),
        Some(serde_json::Value::Object(fields)) => fields.keys().next().cloned(),
        _ => None,
    })
}

fn dismissed_flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Bool(flag)) => flag,
        Some(serde_json::Value::String(mark)) => mark == "dismissed",
        _ => false,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproduceView {
    pub input: String,
    pub expected: String,
    pub actual: String,
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreventionRuleView {
    pub text: String,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingSourceView {
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedView {
    pub finding: FindingView,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementsView {
    pub requirements: TicketView,
    #[serde(default)]
    pub assessments: Vec<AssessmentView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketView {
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub criteria: Vec<CriterionView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionView {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub inferred: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentView {
    pub criterion_id: String,
    pub status: CriterionStatus,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub evidence: Vec<EvidenceView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnverifiableReason>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retried: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human: Option<HumanMark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceView {
    pub file: String,
    pub line: u32,
    #[serde(default)]
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckView {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    pub outcome: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub output_tail: String,
}

impl CheckView {
    pub fn is_failed(&self) -> bool {
        self.outcome != "passed"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateView {
    pub passed: bool,
    #[serde(default)]
    pub blocking: Vec<usize>,
    #[serde(default)]
    pub unmet_criteria: Vec<String>,
    #[serde(default)]
    pub failed_checks: Vec<String>,
    #[serde(default)]
    pub incomplete: bool,
    #[serde(default)]
    pub failed_tests: Vec<String>,
    #[serde(default)]
    pub advisory_tests: Vec<String>,
    #[serde(default)]
    pub rule_violations: Vec<String>,
    #[serde(default)]
    pub design_mismatches: Vec<String>,
    #[serde(default)]
    pub unverified_criteria: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageView {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::finding::fixtures::finding;
    use crate::review::domain::finding::{Demotion, Reproduce};

    #[test]
    fn runs_stored_before_unverifiable_existed_still_load() {
        let old = serde_json::json!({
            "gate": {"passed": true},
            "requirements": {"requirements": {"key": "K"}, "assessments": [
                {"criterion_id": "K/AC-1", "status": "unclear"}]}
        });
        let view: ReportView = serde_json::from_value(old).unwrap();
        let assessment = &view.requirements.unwrap().assessments[0];
        assert_eq!((assessment.reason, assessment.retried, assessment.human), (None, false, None));
        assert!(view.unverifiable.is_empty() && view.notes.is_empty());
        assert!(view.gate.unverified_criteria.is_empty());
    }

    #[test]
    fn unverifiable_items_and_human_marks_read_into_the_view() {
        let json = serde_json::json!({
            "gate": {"passed": true, "unverified_criteria": ["K/AC-1"]},
            "notes": ["retried once"],
            "unverifiable": [{"subject": "criterion", "id": "K/AC-1", "reason": "no_evidence",
                              "detail": "d", "retried": true, "remedy": "r"}],
            "requirements": {"requirements": {"key": "K"}, "assessments": [
                {"criterion_id": "K/AC-1", "status": "met", "human": "verified_met",
                 "reason": "no_evidence", "retried": true}]}
        });
        let view: ReportView = serde_json::from_value(json).unwrap();
        assert_eq!(view.unverifiable[0].reason, UnverifiableReason::NoEvidence);
        assert!(view.unverifiable[0].retried);
        let assessment = &view.requirements.as_ref().unwrap().assessments[0];
        assert_eq!(assessment.human, Some(HumanMark::VerifiedMet));
        assert_eq!(view.gate.unverified_criteria, vec!["K/AC-1"]);
        assert_eq!(view.notes, vec!["retried once"]);
    }

    #[test]
    fn criterion_inferred_defaults_to_false_and_reads_true() {
        let old: CriterionView = serde_json::from_str(r#"{"id":"K/AC-1","text":"t"}"#).unwrap();
        let inferred: CriterionView = serde_json::from_str(r#"{"id":"K/AC-I1","text":"t","inferred":true}"#).unwrap();
        assert!(!old.inferred);
        assert!(inferred.inferred);
    }

    #[test]
    fn live_finding_with_reproduce_and_demotion_reads_into_the_view() {
        let live = crate::review::application::Finding {
            reproduce: Reproduce::new("f(0)", "0", "panic", "go test ./f"),
            demoted: Some(Demotion::MissingReproduce),
            ..finding("a.go", 1, Severity::Major, "x")
        };
        let view: FindingView = serde_json::from_value(serde_json::to_value(&live).unwrap()).unwrap();
        let reproduce = view.reproduce.unwrap();
        assert_eq!((reproduce.input.as_str(), reproduce.command.as_deref()), ("f(0)", Some("go test ./f")));
        assert_eq!(view.demoted.as_deref(), Some("missing_reproduce"));
    }

    #[test]
    fn findings_stored_before_reproduce_existed_still_read() {
        let old = serde_json::json!({"file": "a.go", "line": 1, "severity": "critical", "category": "security",
                                     "confidence": "high", "title": "t"});
        let view: FindingView = serde_json::from_value(old).unwrap();
        assert_eq!((view.reproduce, view.demoted, view.dismissed), (None, None, false));
    }

    #[test]
    fn dismissed_mark_and_parameterised_demotion_read_into_the_view() {
        use crate::review::application::FeedbackMark;
        let live = crate::review::application::Finding {
            feedback: Some(FeedbackMark::Dismissed),
            demoted: Some(Demotion::LowPrecision { score: 30 }),
            ..finding("a.go", 1, Severity::Minor, "x")
        };
        let view: FindingView = serde_json::from_value(serde_json::to_value(&live).unwrap()).unwrap();
        assert!(view.dismissed);
        assert_eq!(view.demoted.as_deref(), Some("low_precision"));
        let again: FindingView = serde_json::from_value(serde_json::to_value(&view).unwrap()).unwrap();
        assert!(again.dismissed);
    }
}
