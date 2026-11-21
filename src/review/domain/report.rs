use std::ops::AddAssign;

use serde::Serialize;

use super::agent_results::{AgentReport, TicketContext};
use super::checks::CheckResult;
use super::finding::Finding;
use super::gate::GateDecision;
use super::requirements::RequirementsReport;
use super::verification::RejectedFinding;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum SkipReason {
    Deleted,
    Binary,
    Ignored,
    TooLarge,
    NoContentChange,
    ReviewFailed(String),
}

impl SkipReason {
    pub fn describe(&self) -> String {
        match self {
            Self::Deleted => "deleted".into(),
            Self::Binary => "binary".into(),
            Self::Ignored => "ignored by config".into(),
            Self::TooLarge => "too large".into(),
            Self::NoContentChange => "no content change".into(),
            Self::ReviewFailed(reason) => format!("review failed: {reason}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedFile {
    pub path: String,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReviewReport {
    pub files_reviewed: Vec<String>,
    pub skipped: Vec<SkippedFile>,
    pub findings: Vec<Finding>,
    pub rejected: Vec<RejectedFinding>,
    pub requirements: Option<RequirementsReport>,
    pub checks: Vec<CheckResult>,
    pub agents: Option<AgentReport>,
    pub ticket: Option<TicketContext>,
    pub gate: GateDecision,
    pub usage: TokenUsage,
    pub estimated_cost_usd: Option<f64>,
}

impl ReviewReport {
    pub fn is_incomplete(&self) -> bool {
        self.skipped.iter().any(|s| matches!(s.reason, SkipReason::ReviewFailed(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_accumulates() {
        let mut total = TokenUsage::default();
        total += TokenUsage { input_tokens: 1, output_tokens: 2, cache_read_tokens: 3, cache_write_tokens: 4 };
        total += TokenUsage { input_tokens: 1, output_tokens: 1, cache_read_tokens: 1, cache_write_tokens: 1 };
        assert_eq!(
            total,
            TokenUsage { input_tokens: 2, output_tokens: 3, cache_read_tokens: 4, cache_write_tokens: 5 }
        );
    }

    #[test]
    fn report_is_incomplete_only_when_a_review_failed() {
        let mut report = ReviewReport {
            files_reviewed: vec![],
            skipped: vec![SkippedFile { path: "a".into(), reason: SkipReason::Binary }],
            findings: vec![],
            rejected: vec![],
            requirements: None,
            checks: vec![],
            agents: None,
            ticket: None,
            gate: GateDecision {
                passed: true,
                blocking: vec![],
                unmet_criteria: vec![],
                failed_checks: vec![],
                incomplete: false,
                ..GateDecision::default()
            },
            usage: TokenUsage::default(),
            estimated_cost_usd: None,
        };
        assert!(!report.is_incomplete());
        report.skipped.push(SkippedFile { path: "b".into(), reason: SkipReason::ReviewFailed("x".into()) });
        assert!(report.is_incomplete());
        assert_eq!(report.skipped[1].reason.describe(), "review failed: x");
    }
}
