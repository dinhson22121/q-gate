use super::case::{CaseCriterion, CaseTicket, ExpectedFinding, ExpectedStatus, ForbiddenFinding, GoldenCase};
use super::scoring::{CaseScore, ExpectedOutcome, ObservedFinding};
use crate::review::application::{Category, Severity};

pub fn case_with(expected: Vec<ExpectedFinding>, forbidden: Vec<ForbiddenFinding>) -> GoldenCase {
    GoldenCase {
        id: "case".into(),
        description: String::new(),
        language: "go".into(),
        ticket: None,
        criteria: Vec::new(),
        expected,
        forbidden,
    }
}

pub fn case_with_criteria(expects: &[Option<ExpectedStatus>]) -> GoldenCase {
    GoldenCase {
        ticket: Some(CaseTicket { key: "KAN-1".into(), title: "t".into(), description: String::new() }),
        criteria: expects
            .iter()
            .enumerate()
            .map(|(i, expect)| CaseCriterion { text: format!("criterion {i}"), expect: *expect })
            .collect(),
        ..case_with(vec![], vec![])
    }
}

pub fn finding(file: &str, line: u32, category: Category, severity: Severity) -> ObservedFinding {
    ObservedFinding { file: file.into(), line, category, severity, title: String::new(), explanation: String::new() }
}

pub fn score(case: &str, repetition: u32, caught: &[bool], fp: usize) -> CaseScore {
    CaseScore {
        case_id: case.into(),
        repetition,
        content_hash: format!("hash-{case}"),
        expected: caught
            .iter()
            .enumerate()
            .map(|(i, caught)| ExpectedOutcome {
                label: format!("a.go:{i} security"),
                category: Category::Security,
                caught: *caught,
                matched_line: None,
            })
            .collect(),
        forbidden_hits: (0..fp).map(|_| finding("a.go", 1, Category::Correctness, Severity::Minor)).collect(),
        unlabeled: Vec::new(),
        criteria: Vec::new(),
        duration_ms: 100,
        cost_usd: 0.5,
        tests: None,
    }
}
