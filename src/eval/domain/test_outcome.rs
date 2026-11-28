use serde::{Deserialize, Serialize};

use crate::review::application::{AgentReport, TestVerdict, UnverifiableReason};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TestsOutcome {
    pub ran: bool,
    pub scenarios: u32,
    pub passed: u32,
    pub failed: u32,
    pub inconclusive: u32,
    pub cost_usd: f64,
    pub notes: Vec<String>,
}

impl TestsOutcome {
    pub fn from_report(report: Option<&AgentReport>) -> Self {
        let Some(report) = report else {
            return Self { notes: vec!["no agent report was produced".into()], ..Self::default() };
        };
        let count = |verdict| report.tests.iter().filter(|t| t.verdict == verdict).count() as u32;
        let executed = report
            .tests
            .iter()
            .any(|t| t.verdict != TestVerdict::Inconclusive || t.reason != Some(UnverifiableReason::TestNotRunnable));
        let mut notes = report.notes.clone();
        notes.extend(
            report
                .tests
                .iter()
                .filter(|t| t.verdict == TestVerdict::Inconclusive && !t.note.is_empty())
                .map(|t| format!("{}: {}", t.scenario_id, t.note)),
        );
        if report.tests.is_empty() && notes.is_empty() {
            notes.push("no test scenarios were produced".into());
        }
        Self {
            ran: executed,
            scenarios: report.tests.len() as u32,
            passed: count(TestVerdict::Passed),
            failed: count(TestVerdict::FailedAssertion),
            inconclusive: count(TestVerdict::Inconclusive),
            cost_usd: report.cost_usd.unwrap_or(0.0),
            notes,
        }
    }

    pub fn summary(&self) -> String {
        if self.ran {
            return format!(
                "ran {} scenario(s): {} passed, {} failed, {} inconclusive · ${:.2}",
                self.scenarios, self.passed, self.failed, self.inconclusive, self.cost_usd
            );
        }
        let reason = if self.notes.is_empty() { "no reason reported".to_string() } else { self.notes.join("; ") };
        format!("skipped: {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::application::TestResult;

    fn result(id: &str, verdict: TestVerdict, reason: Option<UnverifiableReason>, note: &str) -> TestResult {
        TestResult { scenario_id: id.into(), language: "go".into(), verdict, evidence: None, note: note.into(), reason }
    }

    fn report(tests: Vec<TestResult>, notes: &[&str]) -> AgentReport {
        AgentReport {
            tests,
            notes: notes.iter().map(|n| (*n).into()).collect(),
            cost_usd: Some(0.25),
            ..AgentReport::default()
        }
    }

    #[test]
    fn missing_agent_report_is_skipped_with_reason() {
        let outcome = TestsOutcome::from_report(None);

        assert!(!outcome.ran);
        assert_eq!(outcome.summary(), "skipped: no agent report was produced");
    }

    #[test]
    fn counts_verdicts_and_marks_ran() {
        let tests = vec![
            result("S1", TestVerdict::Passed, None, ""),
            result("S2", TestVerdict::FailedAssertion, None, ""),
            result("S3", TestVerdict::Inconclusive, Some(UnverifiableReason::TestTimeout), "timed out"),
        ];

        let outcome = TestsOutcome::from_report(Some(&report(tests, &[])));

        assert!(outcome.ran);
        assert_eq!((outcome.scenarios, outcome.passed, outcome.failed, outcome.inconclusive), (3, 1, 1, 1));
        assert_eq!(outcome.summary(), "ran 3 scenario(s): 1 passed, 1 failed, 1 inconclusive · $0.25");
    }

    #[test]
    fn all_not_runnable_is_skipped_with_scenario_notes() {
        let tests =
            vec![result("S1", TestVerdict::Inconclusive, Some(UnverifiableReason::TestNotRunnable), "no go module")];

        let outcome = TestsOutcome::from_report(Some(&report(tests, &[])));

        assert!(!outcome.ran);
        assert_eq!(outcome.summary(), "skipped: S1: no go module");
    }

    #[test]
    fn no_scenarios_keeps_agent_notes() {
        let outcome = TestsOutcome::from_report(Some(&report(Vec::new(), &["test agents skipped: no scenarios"])));

        assert!(!outcome.ran);
        assert_eq!(outcome.summary(), "skipped: test agents skipped: no scenarios");
    }

    #[test]
    fn no_scenarios_and_no_notes_still_gives_a_reason() {
        let outcome = TestsOutcome::from_report(Some(&report(Vec::new(), &[])));

        assert_eq!(outcome.summary(), "skipped: no test scenarios were produced");
    }

    #[test]
    fn old_scores_without_tests_deserialize() {
        let outcome: Option<TestsOutcome> = serde_json::from_str("null").unwrap();

        assert!(outcome.is_none());
    }
}
