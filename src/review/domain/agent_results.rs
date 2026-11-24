use serde::{Deserialize, Serialize};

use super::change_set::ChangeSet;
use super::requirements::EvidenceRef;
use super::verification::evidence_exists;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TicketContext {
    pub keys: Vec<String>,
    pub description: String,
    pub design_links: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestScenario {
    pub id: String,
    pub criterion_ids: Vec<String>,
    pub title: String,
    pub given: String,
    pub when: String,
    pub then: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TicketBrief {
    pub scenarios: Vec<TestScenario>,
    pub implicit_requirements: Vec<String>,
    pub context: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestVerdict {
    Passed,
    FailedAssertion,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEvidence {
    pub criterion_ids: Vec<String>,
    pub criterion_texts: Vec<String>,
    pub scenario: TestScenario,
    pub test_file: String,
    pub test_source: String,
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub output: String,
    pub code_under_test: Vec<EvidenceRef>,
    pub commit: String,
    pub reproduce: String,
}

impl TestEvidence {
    pub fn missing_parts(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        let mut need = |ok: bool, name: &'static str| {
            if !ok {
                missing.push(name);
            }
        };
        need(!self.criterion_ids.is_empty(), "acceptance criterion id");
        need(self.criterion_texts.iter().any(|t| !t.trim().is_empty()), "acceptance criterion text");
        need(!self.scenario.then.trim().is_empty(), "scenario");
        need(!self.test_file.trim().is_empty(), "test file path");
        need(!self.test_source.trim().is_empty(), "test source");
        need(!self.command.is_empty(), "command");
        need(self.exit_code.is_some(), "exit code");
        need(!self.output.trim().is_empty(), "failing output");
        need(!self.code_under_test.is_empty(), "code under test");
        need(!self.commit.trim().is_empty(), "commit sha");
        need(!self.reproduce.trim().is_empty(), "reproduce command");
        missing
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    pub scenario_id: String,
    pub language: String,
    pub verdict: TestVerdict,
    pub evidence: Option<TestEvidence>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleViolation {
    pub rule_id: String,
    pub rule_title: String,
    pub approved: bool,
    pub file: String,
    pub line: u32,
    pub evidence: String,
    pub explanation: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignMismatchKind {
    Copy,
    Component,
    Token,
    States,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignMismatch {
    pub kind: DesignMismatchKind,
    pub frame: String,
    pub node_id: String,
    pub design_value: String,
    pub file: String,
    pub line: u32,
    pub code_evidence: String,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleComplianceStatus {
    Compliant,
    Violated,
}

impl RuleComplianceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compliant => "compliant",
            Self::Violated => "violated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleAssessment {
    pub rule_id: String,
    pub rule_title: String,
    pub status: RuleComplianceStatus,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgentReport {
    pub brief: Option<TicketBrief>,
    pub tests: Vec<TestResult>,
    pub violations: Vec<RuleViolation>,
    #[serde(default)]
    pub rule_assessments: Vec<RuleAssessment>,
    pub design: Vec<DesignMismatch>,
    pub notes: Vec<String>,
    pub cost_usd: Option<f64>,
}

pub fn verify_agent_report(changes: &ChangeSet, mut report: AgentReport) -> AgentReport {
    for test in &mut report.tests {
        if test.verdict != TestVerdict::FailedAssertion {
            continue;
        }
        let missing = test.evidence.as_ref().map_or_else(|| vec!["all evidence"], TestEvidence::missing_parts);
        if !missing.is_empty() {
            test.verdict = TestVerdict::Inconclusive;
            test.note = format!("downgraded to inconclusive: missing {}", missing.join(", "));
        }
    }
    report.violations.retain(|v| evidence_exists(changes, &v.file, v.line, &v.evidence));
    report.design.retain(|d| evidence_exists(changes, &d.file, d.line, &d.code_evidence));
    let violated: std::collections::HashSet<&str> = report.violations.iter().map(|v| v.rule_id.as_str()).collect();
    for assessment in &mut report.rule_assessments {
        if assessment.status == RuleComplianceStatus::Violated && !violated.contains(assessment.rule_id.as_str()) {
            assessment.status = RuleComplianceStatus::Compliant;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, file};

    fn changes() -> ChangeSet {
        ChangeSet {
            files: vec![file("pay.go", 1, vec![added(1, "total := price * qty")], Some("total := price * qty\n"))],
        }
    }

    fn scenario() -> TestScenario {
        TestScenario {
            id: "S1".into(),
            criterion_ids: vec!["SHOP-7/AC-1".into()],
            title: "total".into(),
            given: "a cart".into(),
            when: "checkout".into(),
            then: "total includes tax".into(),
        }
    }

    fn evidence() -> TestEvidence {
        TestEvidence {
            criterion_ids: vec!["SHOP-7/AC-1".into()],
            criterion_texts: vec!["Total includes tax".into()],
            scenario: scenario(),
            test_file: "pay/qgate_s1_test.go".into(),
            test_source: "func TestS1(t *testing.T) {}".into(),
            command: vec!["go".into(), "test".into()],
            exit_code: Some(1),
            output: "expected 110, got 100".into(),
            code_under_test: vec![EvidenceRef { file: "pay.go".into(), line: 1, quote: "total := price * qty".into() }],
            commit: "abc1234".into(),
            reproduce: "go test ./pay -run TestS1".into(),
        }
    }

    fn failed(evidence: Option<TestEvidence>) -> TestResult {
        TestResult {
            scenario_id: "S1".into(),
            language: "go".into(),
            verdict: TestVerdict::FailedAssertion,
            evidence,
            note: String::new(),
        }
    }

    #[test]
    fn complete_failure_evidence_is_kept() {
        let report = verify_agent_report(
            &changes(),
            AgentReport { tests: vec![failed(Some(evidence()))], ..AgentReport::default() },
        );
        assert_eq!(report.tests[0].verdict, TestVerdict::FailedAssertion);
        assert!(evidence().missing_parts().is_empty());
    }

    #[test]
    fn failure_with_missing_evidence_becomes_inconclusive() {
        let mut partial = evidence();
        partial.reproduce.clear();
        partial.exit_code = None;
        let report = verify_agent_report(
            &changes(),
            AgentReport { tests: vec![failed(Some(partial)), failed(None)], ..AgentReport::default() },
        );
        assert_eq!(report.tests[0].verdict, TestVerdict::Inconclusive);
        assert!(report.tests[0].note.contains("exit code"));
        assert!(report.tests[0].note.contains("reproduce command"));
        assert_eq!(report.tests[1].verdict, TestVerdict::Inconclusive);
    }

    #[test]
    fn violations_and_mismatches_need_real_code_evidence() {
        let violation = |quote: &str| RuleViolation {
            rule_id: "R1".into(),
            rule_title: "no float money".into(),
            approved: true,
            file: "pay.go".into(),
            line: 1,
            evidence: quote.into(),
            explanation: String::new(),
            suggestion: None,
        };
        let mismatch = |quote: &str| DesignMismatch {
            kind: DesignMismatchKind::Copy,
            frame: "Checkout".into(),
            node_id: "1:2".into(),
            design_value: "Pay now".into(),
            file: "pay.go".into(),
            line: 1,
            code_evidence: quote.into(),
            explanation: String::new(),
        };
        let assess = |id: &str, status: RuleComplianceStatus| RuleAssessment {
            rule_id: id.into(),
            rule_title: format!("{id} title"),
            status,
        };
        let report = verify_agent_report(
            &changes(),
            AgentReport {
                violations: vec![violation("total := price * qty"), violation("invented code here")],
                rule_assessments: vec![
                    assess("R1", RuleComplianceStatus::Violated),
                    assess("R9", RuleComplianceStatus::Violated),
                    assess("R2", RuleComplianceStatus::Compliant),
                ],
                design: vec![mismatch("total := price * qty"), mismatch("nothing like this")],
                ..AgentReport::default()
            },
        );
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.design.len(), 1);
        let statuses: Vec<_> = report.rule_assessments.iter().map(|a| (a.rule_id.as_str(), a.status)).collect();
        assert_eq!(
            statuses,
            vec![
                ("R1", RuleComplianceStatus::Violated),
                ("R9", RuleComplianceStatus::Compliant),
                ("R2", RuleComplianceStatus::Compliant),
            ],
            "an assessment loses Violated when its only violation fails grounding"
        );
    }
}
