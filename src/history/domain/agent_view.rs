use serde::{Deserialize, Serialize};

use super::report_view::EvidenceView;
use crate::review::application::{DesignMismatchKind, TestVerdict};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketContextView {
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub design_links: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentReportView {
    #[serde(default)]
    pub brief: Option<TicketBriefView>,
    #[serde(default)]
    pub tests: Vec<TestResultView>,
    #[serde(default)]
    pub violations: Vec<RuleViolationView>,
    #[serde(default)]
    pub design: Vec<DesignMismatchView>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketBriefView {
    #[serde(default)]
    pub scenarios: Vec<TestScenarioView>,
    #[serde(default)]
    pub implicit_requirements: Vec<String>,
    #[serde(default)]
    pub context: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestScenarioView {
    pub id: String,
    #[serde(default)]
    pub criterion_ids: Vec<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub given: String,
    #[serde(default)]
    pub when: String,
    #[serde(default)]
    pub then: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResultView {
    pub scenario_id: String,
    #[serde(default)]
    pub language: String,
    pub verdict: TestVerdict,
    #[serde(default)]
    pub evidence: Option<TestEvidenceView>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEvidenceView {
    #[serde(default)]
    pub criterion_ids: Vec<String>,
    #[serde(default)]
    pub criterion_texts: Vec<String>,
    #[serde(default)]
    pub scenario: Option<TestScenarioView>,
    #[serde(default)]
    pub test_file: String,
    #[serde(default)]
    pub test_source: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub code_under_test: Vec<EvidenceView>,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub reproduce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleViolationView {
    pub rule_id: String,
    #[serde(default)]
    pub rule_title: String,
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignMismatchView {
    pub kind: DesignMismatchKind,
    #[serde(default)]
    pub frame: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub design_value: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub code_evidence: String,
    #[serde(default)]
    pub explanation: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::history::domain::ReportView;

    #[test]
    fn report_written_before_agents_still_loads() {
        let old = json!({
            "files_reviewed": ["a.rs"],
            "gate": { "passed": true, "blocking": [], "unmet_criteria": [], "failed_checks": [] },
            "estimated_cost_usd": 0.1
        });
        let report: ReportView = serde_json::from_value(old).unwrap();
        assert!(report.agents.is_none() && report.ticket.is_none());
        assert!(report.gate.failed_tests.is_empty());
        assert!(report.gate.rule_violations.is_empty() && report.gate.design_mismatches.is_empty());
    }

    fn new_report() -> serde_json::Value {
        json!({
            "gate": {
                "passed": false, "blocking": [], "unmet_criteria": ["SHOP-7/AC-1"], "failed_checks": [],
                "incomplete": false, "failed_tests": ["S1"], "rule_violations": ["R1"], "design_mismatches": ["copy:12:345"]
            },
            "ticket": { "keys": ["SHOP-8", "SHOP-7"], "description": "Pay", "design_links": ["https://www.figma.com/file/x"] },
            "agents": {
                "brief": {
                    "scenarios": [{ "id": "S1", "criterion_ids": ["SHOP-7/AC-1"], "title": "total",
                                    "given": "a cart", "when": "checkout", "then": "tax included" }],
                    "implicit_requirements": ["rounding"], "context": "ctx"
                },
                "tests": [
                    { "scenario_id": "S1", "language": "go", "verdict": "failed_assertion", "note": "",
                      "evidence": {
                          "criterion_ids": ["SHOP-7/AC-1"], "criterion_texts": ["Total includes tax"],
                          "scenario": { "id": "S1", "criterion_ids": ["SHOP-7/AC-1"], "title": "total",
                                        "given": "a cart", "when": "checkout", "then": "tax included" },
                          "test_file": "pay/qgate_s1_test.go", "test_source": "func TestS1(t *testing.T) {}",
                          "command": ["go", "test"], "exit_code": 1, "output": "expected 110, got 100",
                          "code_under_test": [{ "file": "pay.go", "line": 1, "quote": "total := price * qty" }],
                          "commit": "abc1234", "reproduce": "go test ./pay -run TestS1"
                      } },
                    { "scenario_id": "S2", "language": "go", "verdict": "inconclusive", "evidence": null,
                      "note": "budget exhausted" }
                ],
                "violations": [{ "rule_id": "R1", "rule_title": "no float money", "approved": true, "file": "pay.go",
                                 "line": 1, "evidence": "total := price * qty", "explanation": "e", "suggestion": null }],
                "design": [{ "kind": "copy", "frame": "Checkout", "node_id": "12:345", "design_value": "Pay now",
                             "file": "pay.go", "line": 1, "code_evidence": "\"Pay\"", "explanation": "e" }],
                "notes": ["design agent skipped: no Figma link"],
                "cost_usd": 0.31
            }
        })
    }

    #[test]
    fn report_with_agents_loads_every_part() {
        let report: ReportView = serde_json::from_value(new_report()).unwrap();
        assert_eq!(report.gate.failed_tests, vec!["S1"]);
        assert_eq!(report.gate.design_mismatches, vec!["copy:12:345"]);
        assert_eq!(report.ticket.as_ref().unwrap().keys, vec!["SHOP-8", "SHOP-7"]);
        let agents = report.agents.as_ref().unwrap();
        assert_eq!(agents.brief.as_ref().unwrap().scenarios[0].then, "tax included");
        assert_eq!(agents.tests[0].verdict, TestVerdict::FailedAssertion);
        let evidence = agents.tests[0].evidence.as_ref().unwrap();
        assert_eq!((evidence.exit_code, evidence.code_under_test[0].line), (Some(1), 1));
        assert_eq!(evidence.scenario.as_ref().unwrap().id, "S1");
        assert!(agents.tests[1].evidence.is_none());
        assert!(agents.violations[0].approved && agents.violations[0].suggestion.is_none());
        assert_eq!(agents.design[0].kind, DesignMismatchKind::Copy);
        assert_eq!(agents.cost_usd, Some(0.31));

        let round_trip: ReportView = serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(round_trip, report);
    }

    #[test]
    fn partial_agent_sections_default_to_empty() {
        let report: ReportView =
            serde_json::from_value(json!({ "gate": { "passed": true }, "agents": {}, "ticket": {} })).unwrap();
        assert_eq!(report.agents, Some(AgentReportView::default()));
        assert_eq!(report.ticket, Some(TicketContextView::default()));
    }

    #[test]
    fn live_agent_report_serializes_into_the_read_model() {
        use crate::review::application::{AgentReport, TicketContext};
        let live = AgentReport { notes: vec!["skipped".into()], cost_usd: Some(0.2), ..AgentReport::default() };
        let view: AgentReportView = serde_json::from_value(serde_json::to_value(&live).unwrap()).unwrap();
        assert_eq!((view.notes, view.cost_usd), (vec!["skipped".to_string()], Some(0.2)));
        let ctx = TicketContext { keys: vec!["SHOP-7".into()], ..TicketContext::default() };
        let view: TicketContextView = serde_json::from_value(serde_json::to_value(&ctx).unwrap()).unwrap();
        assert_eq!(view.keys, vec!["SHOP-7"]);
    }
}
