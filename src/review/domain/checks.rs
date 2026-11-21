//! Deterministic verification commands (tests, linters, builds) declared by the repository.
//! These are the strongest layer: a failing required check fails the gate regardless of the LLM.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckSpec {
    pub name: String,
    /// Program and arguments, executed without a shell.
    pub command: Vec<String>,
    /// A failing required check fails the gate; optional checks are informational.
    pub required: bool,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Passed,
    Failed,
    TimedOut,
    /// The program could not be started (not installed, bad path...).
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub required: bool,
    pub outcome: CheckOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// Last lines of combined stdout/stderr, for diagnosis.
    pub output_tail: String,
}

impl CheckResult {
    pub fn is_blocking(&self) -> bool {
        self.required && self.outcome != CheckOutcome::Passed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(required: bool, outcome: CheckOutcome) -> CheckResult {
        CheckResult { name: "t".into(), required, outcome, exit_code: None, duration_ms: 0, output_tail: String::new() }
    }

    #[test]
    fn only_required_non_passing_checks_block() {
        assert!(result(true, CheckOutcome::Failed).is_blocking());
        assert!(result(true, CheckOutcome::TimedOut).is_blocking());
        assert!(result(true, CheckOutcome::Error).is_blocking());
        assert!(!result(true, CheckOutcome::Passed).is_blocking());
        assert!(!result(false, CheckOutcome::Failed).is_blocking());
    }
}
