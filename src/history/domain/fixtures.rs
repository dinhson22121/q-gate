use chrono::{DateTime, TimeZone, Utc};

use super::report_view::*;
use super::run::{ReviewRun, RunStatus, StoredRun, Trigger};
use crate::review::application::{
    AcceptanceCriterion, Category, CheckOutcome, CheckResult, Confidence, CriterionStatus, Finding, PreventionRule,
    Requirements, RequirementsReport, ReviewReport, Severity, SkipReason, TokenUsage,
};
use crate::review::domain::finding::FindingSource;
use crate::review::domain::gate::GateDecision;
use crate::review::domain::report::SkippedFile;
use crate::review::domain::requirements::{CriterionAssessment, EvidenceRef};
use crate::review::domain::verification::{RejectReason, RejectedFinding};

pub fn at(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, day, hour, 0, 0).unwrap()
}

pub fn finding(severity: Severity, category: Category, title: &str) -> FindingView {
    FindingView {
        file: "src/lib.rs".into(),
        line: 10,
        severity,
        category,
        confidence: Confidence::High,
        title: title.into(),
        explanation: "why".into(),
        evidence: "let x = y.unwrap();".into(),
        suggestion: None,
        prevention_rule: None,
        source: None,
        reproduce: None,
        demoted: None,
    }
}

pub fn report_view(findings: Vec<FindingView>, criteria: &[CriterionStatus], failed_checks: usize) -> ReportView {
    ReportView {
        files_reviewed: vec!["src/lib.rs".into()],
        skipped: Vec::new(),
        findings,
        rejected: Vec::new(),
        requirements: (!criteria.is_empty()).then(|| RequirementsView {
            requirements: TicketView { key: "QG-1".into(), title: "Ticket".into(), url: None, criteria: Vec::new() },
            assessments: criteria
                .iter()
                .enumerate()
                .map(|(i, s)| AssessmentView {
                    criterion_id: format!("AC{i}"),
                    status: *s,
                    explanation: String::new(),
                    evidence: Vec::new(),
                })
                .collect(),
        }),
        checks: (0..failed_checks)
            .map(|i| CheckView {
                name: format!("check{i}"),
                required: true,
                outcome: "failed".into(),
                exit_code: Some(1),
                duration_ms: 5,
                output_tail: String::new(),
            })
            .collect(),
        gate: GateView {
            passed: true,
            blocking: Vec::new(),
            unmet_criteria: Vec::new(),
            failed_checks: Vec::new(),
            incomplete: false,
            failed_tests: Vec::new(),
            advisory_tests: Vec::new(),
            rule_violations: Vec::new(),
            design_mismatches: Vec::new(),
        },
        usage: UsageView::default(),
        estimated_cost_usd: Some(0.25),
        agents: None,
        ticket: None,
        audited: false,
    }
}

pub fn with_criteria(requirements_key: &str, criteria: &[(&str, CriterionStatus)]) -> ReportView {
    let mut report = report_view(Vec::new(), &[], 0);
    report.requirements = Some(RequirementsView {
        requirements: TicketView {
            key: requirements_key.into(),
            title: String::new(),
            url: None,
            criteria: criteria
                .iter()
                .map(|(id, _)| CriterionView { id: (*id).into(), text: format!("text of {id}") })
                .collect(),
        },
        assessments: criteria
            .iter()
            .map(|(id, status)| AssessmentView {
                criterion_id: (*id).into(),
                status: *status,
                explanation: String::new(),
                evidence: Vec::new(),
            })
            .collect(),
    });
    report
}

pub fn stored(
    id: i64,
    repo: &str,
    created_at: DateTime<Utc>,
    status: RunStatus,
    report: Option<ReportView>,
) -> StoredRun {
    ReviewRun {
        id,
        repo_path: format!("/src/{repo}"),
        repo_name: repo.into(),
        commit: Some("0123456789abcdef".into()),
        branch: Some("main".into()),
        ticket_key: None,
        trigger: Trigger::Commit,
        created_at,
        status,
        error: None,
        report,
        report_md: None,
    }
}

fn live_finding(title: &str) -> Finding {
    Finding {
        file: "src/<script>.rs".into(),
        line: 3,
        severity: Severity::Critical,
        category: Category::Security,
        confidence: Confidence::High,
        title: title.into(),
        explanation: "untrusted <b>input</b>".into(),
        evidence: "query(&format!(\"{}\", input))".into(),
        suggestion: Some("bind parameters".into()),
        prevention_rule: Some(PreventionRule { text: "Bind <b>parameters</b>".into(), scope: Some("**/*.rs".into()) }),
        source: FindingSource::Llm,
        reproduce: None,
        demoted: None,
    }
}

pub fn review_report() -> ReviewReport {
    ReviewReport {
        files_reviewed: vec!["src/<script>.rs".into()],
        skipped: vec![SkippedFile { path: "img.png".into(), reason: SkipReason::Binary }],
        findings: vec![live_finding("SQL built from `input`")],
        rejected: vec![RejectedFinding {
            finding: live_finding("hallucinated"),
            reason: RejectReason::EvidenceNotFound,
        }],
        requirements: Some(RequirementsReport {
            requirements: Requirements {
                key: "QG-7".into(),
                title: "Login".into(),
                url: Some("https://jira.example/browse/QG-7".into()),
                criteria: vec![AcceptanceCriterion { id: "AC1".into(), text: "Users can log in".into() }],
            },
            assessments: vec![CriterionAssessment {
                criterion_id: "AC1".into(),
                status: CriterionStatus::NotMet,
                explanation: "no handler".into(),
                evidence: vec![EvidenceRef { file: "src/a.rs".into(), line: 2, quote: "fn x()".into() }],
            }],
        }),
        checks: vec![CheckResult {
            name: "clippy".into(),
            required: true,
            outcome: CheckOutcome::Failed,
            exit_code: Some(101),
            duration_ms: 1200,
            output_tail: "error: unused".into(),
        }],
        gate: GateDecision {
            passed: false,
            blocking: vec![0],
            unmet_criteria: vec!["AC1".into()],
            failed_checks: vec!["clippy".into()],
            incomplete: false,
            ..GateDecision::default()
        },
        usage: TokenUsage { input_tokens: 100, output_tokens: 20, cache_read_tokens: 0, cache_write_tokens: 0 },
        estimated_cost_usd: Some(0.042),
        audited: false,
        agents: None,
        ticket: None,
    }
}

pub fn live_run(repo: &str, created_at: DateTime<Utc>) -> ReviewRun {
    let report = review_report();
    ReviewRun {
        id: 0,
        repo_path: format!("/src/{repo}"),
        repo_name: repo.into(),
        commit: Some("abcdef0123456789".into()),
        branch: Some("feature/x".into()),
        ticket_key: Some("QG-7".into()),
        trigger: Trigger::Manual,
        created_at,
        status: RunStatus::of_report(&report),
        error: None,
        report: Some(report),
        report_md: None,
    }
}
