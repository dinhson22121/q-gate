//! Results of the ticket agents (test agents, architecture conformance, design checks) as the
//! review context sees them, and the evidence rules that decide whether a result may affect the gate.

use serde::{Deserialize, Serialize};

use super::change_set::ChangeSet;
use super::requirements::EvidenceRef;
use super::verification::evidence_exists;

/// Ticket material beyond acceptance criteria: the reviewers use it as context, never as a gate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TicketContext {
    /// Keys whose criteria are being checked (the ticket and its parent Story, if any).
    pub keys: Vec<String>,
    /// Plain-text descriptions (ticket first, then parent Story), already truncated.
    pub description: String,
    /// Figma links found on the ticket or its parent Story.
    pub design_links: Vec<String>,
}

/// A behaviour to verify, derived from acceptance criteria by the ticket analyst.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestScenario {
    /// Stable within one brief, e.g. "S1".
    pub id: String,
    /// Acceptance criteria this scenario proves, e.g. ["SHOP-7/AC-1"].
    pub criterion_ids: Vec<String>,
    pub title: String,
    pub given: String,
    pub when: String,
    pub then: String,
}

/// What the ticket analyst produced for one review.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TicketBrief {
    pub scenarios: Vec<TestScenario>,
    /// Requirements implied by the description but not written as criteria. Informational only.
    pub implicit_requirements: Vec<String>,
    /// Short context for the other reviewers (intent, constraints, non-functional needs).
    pub context: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestVerdict {
    Passed,
    /// The test ran and an assertion about the required behaviour failed.
    FailedAssertion,
    /// Could not decide: the test did not compile, setup failed, budget ran out, or evidence is incomplete.
    Inconclusive,
}

/// Everything a reader needs to check a test result without trusting the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEvidence {
    pub criterion_ids: Vec<String>,
    /// Criterion texts, so the evidence stands on its own.
    pub criterion_texts: Vec<String>,
    pub scenario: TestScenario,
    /// Path of the generated test inside the snapshot, relative to the repository root.
    pub test_file: String,
    pub test_source: String,
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    /// Failing assertion and surrounding output (tail).
    pub output: String,
    /// The production code the test exercises.
    pub code_under_test: Vec<EvidenceRef>,
    pub commit: String,
    /// Shell command a developer can run locally to reproduce.
    pub reproduce: String,
}

impl TestEvidence {
    /// The fixed evidence contract: a failure missing any of these cannot affect the gate.
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
    /// Why the verdict is what it is (e.g. which evidence parts were missing).
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleViolation {
    pub rule_id: String,
    pub rule_title: String,
    /// Approved rules may block; inferred ones never do.
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
    /// Figma frame name and node id, e.g. "Checkout / Payment" + "12:345".
    pub frame: String,
    pub node_id: String,
    /// What the design says (text, component name, variable name and value, variant name).
    pub design_value: String,
    pub file: String,
    pub line: u32,
    pub code_evidence: String,
    pub explanation: String,
}

/// Agent output for one review, after verification.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgentReport {
    pub brief: Option<TicketBrief>,
    pub tests: Vec<TestResult>,
    pub violations: Vec<RuleViolation>,
    pub design: Vec<DesignMismatch>,
    /// Agents that were skipped or failed, with the reason (budget, Jira down, no Figma link...).
    pub notes: Vec<String>,
    pub cost_usd: Option<f64>,
}

/// Downgrades failures without complete evidence, and drops violations / mismatches whose quoted
/// code is not in the change. Returns the verified report.
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
        let report = verify_agent_report(
            &changes(),
            AgentReport {
                violations: vec![violation("total := price * qty"), violation("invented code here")],
                design: vec![mismatch("total := price * qty"), mismatch("nothing like this")],
                ..AgentReport::default()
            },
        );
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.design.len(), 1);
    }
}
