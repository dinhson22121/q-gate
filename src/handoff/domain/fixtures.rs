use chrono::{TimeZone, Utc};

use super::document::RunMeta;
use crate::review::application::{
    AcceptanceCriterion, AgentReport, Category, CheckOutcome, CheckResult, Confidence, CriterionStatus, DesignMismatch,
    DesignMismatchKind, EvidenceRef, Finding, PreventionRule, Requirements, RequirementsReport, ReviewReport,
    RuleAssessment, RuleComplianceStatus, RuleViolation, Severity, TestEvidence, TestResult, TestScenario, TestVerdict,
    TicketBrief, TicketContext, TokenUsage,
};
use crate::review::domain::finding::FindingSource;
use crate::review::domain::gate::GateDecision;
use crate::review::domain::requirements::CriterionAssessment;

pub const SHA: &str = "3f9c2ab7d1e04c5b8a6f9e2d1c0b3a4f5e6d7c8b";
pub const FENCED_EVIDENCE: &str = "query := \"SELECT * FROM t WHERE id=\" + id // see ```sql``` docs\n````";

pub fn meta() -> RunMeta {
    RunMeta {
        repo: "shop-api".into(),
        branch: Some("feature/SHOP-7-pay".into()),
        commit: SHA.into(),
        ticket_keys: vec!["SHOP-7".into()],
        reviewed_at: Utc.with_ymd_and_hms(2026, 11, 20, 14, 30, 5).unwrap(),
        run_id: Some(42),
        dashboard_url: Some("http://localhost:8080/".into()),
    }
}

fn finding(file: &str, line: u32, severity: Severity, category: Category, title: &str, evidence: &str) -> Finding {
    Finding {
        file: file.into(),
        line,
        severity,
        category,
        confidence: Confidence::High,
        title: title.into(),
        explanation: format!("{title} explained."),
        evidence: evidence.into(),
        suggestion: Some(format!("Fix {title}.")),
        prevention_rule: None,
        source: FindingSource::Llm,
        reproduce: None,
        demoted: None,
        feedback: None,
    }
}

fn with_rule(finding: Finding, text: &str, scope: &str) -> Finding {
    Finding { prevention_rule: PreventionRule::new(text, scope), ..finding }
}

fn check(name: &str, required: bool, outcome: CheckOutcome) -> CheckResult {
    CheckResult {
        name: name.into(),
        required,
        outcome,
        exit_code: Some(i32::from(outcome != CheckOutcome::Passed)),
        duration_ms: 1200,
        output_tail: if outcome == CheckOutcome::Passed { String::new() } else { format!("{name}: 1 failure") },
    }
}

fn assessment(id: &str, status: CriterionStatus, evidence: Vec<EvidenceRef>) -> CriterionAssessment {
    CriterionAssessment { criterion_id: id.into(), status, explanation: format!("{id} assessed."), evidence }
}

fn evidence_ref(file: &str, line: u32, quote: &str) -> EvidenceRef {
    EvidenceRef { file: file.into(), line, quote: quote.into() }
}

pub fn requirements() -> RequirementsReport {
    RequirementsReport {
        requirements: Requirements {
            key: "SHOP-7".into(),
            title: "Pay with tax".into(),
            url: Some("https://acme.atlassian.net/browse/SHOP-7".into()),
            criteria: vec![
                AcceptanceCriterion { id: "AC-1".into(), text: "Total includes tax".into() },
                AcceptanceCriterion { id: "AC-2".into(), text: "Receipt | is emailed".into() },
                AcceptanceCriterion { id: "AC-3".into(), text: "Works offline".into() },
            ],
        },
        assessments: vec![
            assessment("AC-1", CriterionStatus::Met, vec![evidence_ref("pay/total.go", 10, "total := net + tax")]),
            assessment("AC-2", CriterionStatus::NotMet, vec![]),
            assessment("AC-3", CriterionStatus::Unclear, vec![]),
        ],
    }
}

pub fn failing_report() -> ReviewReport {
    ReviewReport {
        files_reviewed: vec!["pay/total.go".into(), "pay/db.go".into()],
        skipped: vec![],
        findings: vec![
            with_rule(
                finding("pay/db.go", 42, Severity::Critical, Category::Security, "SQL injection", FENCED_EVIDENCE),
                "Build SQL with bound parameters only.",
                "**/*.go",
            ),
            with_rule(
                finding("pay/total.go", 7, Severity::Major, Category::Maintainability, "Magic number", "rate := 0.2"),
                "Name constants instead of magic numbers.",
                "",
            ),
            with_rule(
                finding("pay/total.go", 1, Severity::Info, Category::Maintainability, "Nit", "package pay"),
                "Info-only rules are not learned",
                "",
            ),
        ],
        rejected: vec![],
        requirements: Some(requirements()),
        checks: vec![
            check("unit", true, CheckOutcome::Failed),
            check("lint", false, CheckOutcome::Failed),
            check("build", true, CheckOutcome::Passed),
        ],
        gate: GateDecision {
            passed: false,
            blocking: vec![0],
            unmet_criteria: vec!["AC-2".into()],
            failed_checks: vec!["unit".into()],
            incomplete: false,
            failed_tests: vec!["S2".into()],
            advisory_tests: Vec::new(),
            rule_violations: vec!["R1".into()],
            design_mismatches: vec!["copy:1:2".into()],
        },
        agents: Some(agents()),
        ticket: Some(ticket()),
        usage: TokenUsage::default(),
        estimated_cost_usd: Some(0.12),
        audited: true,
    }
}

fn scenario(id: &str) -> TestScenario {
    TestScenario {
        id: id.into(),
        criterion_ids: vec!["SHOP-7/AC-1".into()],
        title: "Total includes tax".into(),
        given: "a cart of 100".into(),
        when: "the customer checks out".into(),
        then: "the total is 110".into(),
    }
}

pub fn test_evidence() -> TestEvidence {
    TestEvidence {
        criterion_ids: vec!["SHOP-7/AC-1".into()],
        criterion_texts: vec!["Total includes tax".into()],
        scenario: scenario("S2"),
        test_file: "pay/qgate_s2_test.go".into(),
        test_source: "func TestS2(t *testing.T) {\n\tif Total(100) != 110 { t.Fatal(\"want 110\") }\n}".into(),
        command: vec!["go".into(), "test".into(), "./pay".into()],
        exit_code: Some(1),
        output: "--- FAIL: TestS2\n    want 110, got 100".into(),
        code_under_test: vec![evidence_ref("pay/total.go", 10, "total := net + tax")],
        commit: SHA.into(),
        reproduce: "go test ./pay -run TestS2".into(),
    }
}

pub fn agents() -> AgentReport {
    let violation = |id: &str, approved: bool, line: u32| RuleViolation {
        rule_id: id.into(),
        rule_title: format!("{id} title"),
        approved,
        file: "pay/db.go".into(),
        line,
        evidence: "db.Exec(query)".into(),
        explanation: format!("{id} explained."),
        suggestion: Some(format!("Follow {id}.")),
    };
    let mismatch = |kind: DesignMismatchKind, node: &str| DesignMismatch {
        kind,
        frame: "Checkout / Pay".into(),
        node_id: node.into(),
        design_value: "Pay now".into(),
        file: "web/Pay.tsx".into(),
        line: 5,
        code_evidence: "<button>Pay</button>".into(),
        explanation: "Label differs.".into(),
    };
    AgentReport {
        brief: Some(TicketBrief {
            scenarios: vec![scenario("S1"), scenario("S2")],
            implicit_requirements: vec!["Amounts use integer cents".into()],
            context: String::new(),
        }),
        tests: vec![
            TestResult {
                scenario_id: "S2".into(),
                language: "go".into(),
                verdict: TestVerdict::FailedAssertion,
                evidence: Some(test_evidence()),
                note: String::new(),
            },
            TestResult {
                scenario_id: "S1".into(),
                language: "go".into(),
                verdict: TestVerdict::Inconclusive,
                evidence: None,
                note: "test did not compile".into(),
            },
        ],
        violations: vec![violation("R2", false, 3), violation("R1", true, 42)],
        rule_assessments: vec![
            RuleAssessment {
                rule_id: "R1".into(),
                rule_title: "R1 title".into(),
                status: RuleComplianceStatus::Violated,
            },
            RuleAssessment {
                rule_id: "R2".into(),
                rule_title: "R2 title".into(),
                status: RuleComplianceStatus::Violated,
            },
            RuleAssessment {
                rule_id: "R5".into(),
                rule_title: "Bound SQL parameters".into(),
                status: RuleComplianceStatus::Compliant,
            },
        ],
        design: vec![mismatch(DesignMismatchKind::States, "1:9"), mismatch(DesignMismatchKind::Copy, "1:2")],
        notes: vec!["architecture: rulebook rebuilt".into()],
        cost_usd: None,
    }
}

pub fn ticket() -> TicketContext {
    TicketContext {
        keys: vec!["SHOP-7".into(), "SHOP-1".into()],
        description: "As a buyer\nI want tax included.".into(),
        design_links: vec!["https://www.figma.com/design/abc/Shop?node-id=1-2".into()],
        notes: Vec::new(),
    }
}

pub fn passing_report() -> ReviewReport {
    ReviewReport {
        files_reviewed: vec!["pay/total.go".into()],
        skipped: vec![],
        findings: vec![],
        rejected: vec![],
        requirements: None,
        checks: vec![check("unit", true, CheckOutcome::Passed)],
        agents: None,
        ticket: None,
        gate: GateDecision { passed: true, ..GateDecision::default() },
        usage: TokenUsage::default(),
        estimated_cost_usd: None,
        audited: true,
    }
}
