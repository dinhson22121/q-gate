//! The verdict of one scenario, decided by qgate from the LAST real run (the model's verdict is
//! advisory), and the evidence assembled from facts qgate recorded itself.

use crate::review::application::{EvidenceRef, Requirements, TestEvidence, TestResult, TestScenario, TestVerdict};

use super::classify::{RawRun, RunClass};
use super::language::TestLanguage;
use super::paths::join;
use super::profile::TestTarget;

/// One execution of the generated test, with the source exactly as it was run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub class: RunClass,
    pub raw: RawRun,
    pub source: String,
}

/// Everything known about a scenario once the agent loop is over.
#[derive(Debug, Clone)]
pub struct ScenarioOutcome<'a> {
    pub scenario: &'a TestScenario,
    pub language: TestLanguage,
    pub criterion_texts: Vec<String>,
    pub commit: &'a str,
    /// Repository root as shown to the developer (used in the reproduce command).
    pub repo_root: &'a str,
    /// The target the test was written to, if any.
    pub target: Option<&'a TestTarget>,
    pub runs: &'a [RunRecord],
    pub max_repair_rounds: u32,
    /// Verified references to production code (quotes checked against the snapshot).
    pub code_under_test: Vec<EvidenceRef>,
    /// The model's own verdict from `report_result`; never decides anything.
    pub advisory: Option<TestVerdict>,
    pub explanation: String,
    /// Extra facts for the note (loop errors, rejected references...).
    pub remarks: Vec<String>,
}

/// An inconclusive result without evidence (nothing ran).
pub fn inconclusive(scenario_id: &str, language: &str, note: impl Into<String>) -> TestResult {
    TestResult {
        scenario_id: scenario_id.to_string(),
        language: language.to_string(),
        verdict: TestVerdict::Inconclusive,
        evidence: None,
        note: note.into(),
    }
}

pub fn decide(outcome: ScenarioOutcome<'_>) -> TestResult {
    let language = outcome.language.as_str();
    let id = outcome.scenario.id.as_str();
    let (Some(target), Some(last)) = (outcome.target, outcome.runs.last()) else {
        return inconclusive(id, language, with_remarks("the test was never run".into(), &outcome));
    };
    let evidence = assemble_evidence(&outcome, target, last);
    let (verdict, reason) = match last.class {
        RunClass::Passed => (TestVerdict::Passed, "the test passed".to_string()),
        RunClass::FailedAssertion => match evidence.missing_parts() {
            missing if missing.is_empty() => (TestVerdict::FailedAssertion, "an assertion failed".to_string()),
            missing => {
                (TestVerdict::Inconclusive, format!("assertion failed but evidence is missing: {}", missing.join(", ")))
            }
        },
        RunClass::NeedsRepair => (
            TestVerdict::Inconclusive,
            format!(
                "the test did not compile or failed without an assertion (repair rounds used: {} of {})",
                outcome.runs.len().saturating_sub(1),
                outcome.max_repair_rounds
            ),
        ),
        RunClass::TimedOut => (TestVerdict::Inconclusive, "the test run timed out".to_string()),
        RunClass::ToolchainMissing => (
            TestVerdict::Inconclusive,
            format!("toolchain missing: install {} via QGATE_TOOLCHAINS", outcome.language.toolchain()),
        ),
        RunClass::SetupFailed => (TestVerdict::Inconclusive, setup_failure(&last.raw)),
    };
    let mut note = reason;
    if let Some(advisory) = outcome.advisory.filter(|a| *a != verdict) {
        note.push_str(&format!("; the agent reported {} (advisory only)", verdict_name(advisory)));
    }
    if !outcome.explanation.trim().is_empty() {
        note.push_str(&format!("; agent: {}", outcome.explanation.trim()));
    }
    TestResult {
        scenario_id: id.to_string(),
        language: language.to_string(),
        verdict,
        evidence: Some(evidence),
        note: with_remarks(note, &outcome),
    }
}

fn setup_failure(run: &RawRun) -> String {
    match run.spawn_error.as_deref() {
        Some(reason) => format!("the test command could not be started: {reason}"),
        None => "the test command could not be started".to_string(),
    }
}

fn with_remarks(mut note: String, outcome: &ScenarioOutcome<'_>) -> String {
    for remark in &outcome.remarks {
        note.push_str("; ");
        note.push_str(remark);
    }
    note
}

fn verdict_name(verdict: TestVerdict) -> &'static str {
    match verdict {
        TestVerdict::Passed => "passed",
        TestVerdict::FailedAssertion => "failed_assertion",
        TestVerdict::Inconclusive => "inconclusive",
    }
}

fn assemble_evidence(outcome: &ScenarioOutcome<'_>, target: &TestTarget, last: &RunRecord) -> TestEvidence {
    TestEvidence {
        criterion_ids: outcome.scenario.criterion_ids.clone(),
        criterion_texts: outcome.criterion_texts.clone(),
        scenario: outcome.scenario.clone(),
        test_file: target.test_file.clone(),
        test_source: last.source.clone(),
        command: target.command.clone(),
        exit_code: last.raw.exit_code,
        output: last.raw.output.clone(),
        code_under_test: outcome.code_under_test.clone(),
        commit: outcome.commit.to_string(),
        reproduce: reproduce_command(outcome.repo_root, target),
    }
}

/// Texts of the scenario's criteria. Ids are `KEY/AC-n` (or a bare `AC-n`); unknown ids are skipped.
pub fn criterion_texts(requirements: Option<&Requirements>, ids: &[String]) -> Vec<String> {
    let Some(requirements) = requirements else {
        return Vec::new();
    };
    ids.iter()
        .filter_map(|id| {
            requirements
                .criteria
                .iter()
                .find(|c| *id == c.id || *id == format!("{}/{}", requirements.key, c.id))
                .map(|c| c.text.clone())
        })
        .collect()
}

/// `cd <repo>/<module> && <command>`, noting that the test file comes from qgate.
pub fn reproduce_command(repo_root: &str, target: &TestTarget) -> String {
    let dir = join(repo_root.trim_end_matches('/'), &target.module_dir);
    let command: Vec<String> = target.command.iter().map(|a| shell_quote(a)).collect();
    format!(
        "cd {} && {}  # first save test_source (generated by qgate) as {}",
        shell_quote(&dir),
        command.join(" "),
        target.test_file
    )
}

/// POSIX single-quoting unless the word only has shell-safe characters.
fn shell_quote(word: &str) -> String {
    let safe = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_-./=:,+@%".contains(c));
    if safe { word.to_string() } else { format!("'{}'", word.replace('\'', "'\\''")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::application::AcceptanceCriterion;

    fn scenario() -> TestScenario {
        TestScenario {
            id: "S1".into(),
            criterion_ids: vec!["SHOP-7/AC-1".into()],
            title: "sum".into(),
            given: "two numbers".into(),
            when: "Add is called".into(),
            then: "it returns the sum".into(),
        }
    }

    fn target() -> TestTarget {
        TestTarget {
            language: TestLanguage::Go,
            module_dir: "svc".into(),
            test_file: "svc/calc/qgate_s1_test.go".into(),
            command: ["go", "test", "./calc", "-run", "^TestQgateS1", "-count=1"].map(String::from).to_vec(),
        }
    }

    fn run(class: RunClass, exit: i32, output: &str) -> RunRecord {
        RunRecord {
            class,
            raw: RawRun { exit_code: Some(exit), output: output.into(), ..RawRun::default() },
            source: "package calc\nfunc TestQgateS1(t *testing.T) {}".into(),
        }
    }

    fn code_ref() -> EvidenceRef {
        EvidenceRef { file: "svc/calc/calc.go".into(), line: 3, quote: "return a - b".into() }
    }

    fn outcome<'a>(scenario: &'a TestScenario, target: &'a TestTarget, runs: &'a [RunRecord]) -> ScenarioOutcome<'a> {
        ScenarioOutcome {
            scenario,
            language: TestLanguage::Go,
            criterion_texts: vec!["Add returns the sum".into()],
            commit: "abc123",
            repo_root: "/repos/shop",
            target: Some(target),
            runs,
            max_repair_rounds: 3,
            code_under_test: vec![code_ref()],
            advisory: None,
            explanation: String::new(),
            remarks: Vec::new(),
        }
    }

    #[test]
    fn failed_assertion_with_complete_evidence() {
        let (scenario, target) = (scenario(), target());
        let runs = [run(RunClass::NeedsRepair, 2, "undefined: X"), run(RunClass::FailedAssertion, 1, "--- FAIL")];
        let result = decide(outcome(&scenario, &target, &runs));
        assert_eq!(result.verdict, TestVerdict::FailedAssertion);
        let evidence = result.evidence.unwrap();
        assert!(evidence.missing_parts().is_empty());
        assert_eq!(evidence.output, "--- FAIL");
        assert_eq!(evidence.exit_code, Some(1));
        assert_eq!(evidence.test_file, "svc/calc/qgate_s1_test.go");
        assert_eq!(evidence.commit, "abc123");
        assert_eq!(
            evidence.reproduce,
            "cd /repos/shop/svc && go test ./calc -run '^TestQgateS1' -count=1  \
             # first save test_source (generated by qgate) as svc/calc/qgate_s1_test.go"
        );
    }

    #[test]
    fn verdict_comes_from_the_run_not_from_the_model() {
        let (scenario, target) = (scenario(), target());
        let runs = [run(RunClass::Passed, 0, "ok")];
        let mut claimed_failure = outcome(&scenario, &target, &runs);
        claimed_failure.advisory = Some(TestVerdict::FailedAssertion);
        claimed_failure.explanation = "the feature is missing".into();
        let result = decide(claimed_failure);
        assert_eq!(result.verdict, TestVerdict::Passed);
        assert!(result.note.contains("agent reported failed_assertion (advisory only)"), "{}", result.note);
        assert!(result.note.contains("the feature is missing"));
    }

    #[test]
    fn failure_with_missing_evidence_is_inconclusive() {
        let (scenario, target) = (scenario(), target());
        let runs = [run(RunClass::FailedAssertion, 1, "--- FAIL")];
        let mut partial = outcome(&scenario, &target, &runs);
        partial.code_under_test.clear();
        partial.criterion_texts.clear();
        partial.advisory = Some(TestVerdict::FailedAssertion);
        let result = decide(partial);
        assert_eq!(result.verdict, TestVerdict::Inconclusive);
        assert!(result.note.contains("acceptance criterion text"), "{}", result.note);
        assert!(result.note.contains("code under test"));
        assert!(result.evidence.is_some(), "partial evidence is still shown");
    }

    #[test]
    fn other_run_classes_are_inconclusive() {
        let (scenario, target) = (scenario(), target());
        for (class, expected) in [
            (RunClass::NeedsRepair, "repair rounds used: 0 of 3"),
            (RunClass::TimedOut, "timed out"),
            (RunClass::ToolchainMissing, "install go via QGATE_TOOLCHAINS"),
            (RunClass::SetupFailed, "could not be started"),
        ] {
            let runs = [run(class, 1, "x")];
            let mut o = outcome(&scenario, &target, &runs);
            o.remarks = vec!["loop stopped".into()];
            let result = decide(o);
            assert_eq!(result.verdict, TestVerdict::Inconclusive);
            assert!(result.note.contains(expected), "{}", result.note);
            assert!(result.note.ends_with("; loop stopped"));
        }
    }

    #[test]
    fn never_run_is_inconclusive_without_evidence() {
        let (scenario, target) = (scenario(), target());
        let result = decide(outcome(&scenario, &target, &[]));
        assert_eq!(result.verdict, TestVerdict::Inconclusive);
        assert!(result.evidence.is_none());
        assert_eq!(result.language, "go");
        assert!(result.note.contains("never run"));
    }

    #[test]
    fn criterion_texts_match_keyed_and_bare_ids() {
        let requirements = Requirements {
            key: "SHOP-7".into(),
            title: "t".into(),
            url: None,
            criteria: vec![
                AcceptanceCriterion { id: "AC-1".into(), text: "one".into() },
                AcceptanceCriterion { id: "AC-2".into(), text: "two".into() },
            ],
        };
        let ids = ["SHOP-7/AC-2".to_string(), "AC-1".to_string(), "OTHER/AC-1".to_string()];
        assert_eq!(criterion_texts(Some(&requirements), &ids), ["two", "one"]);
        assert!(criterion_texts(None, &ids).is_empty());
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote("./calc"), "./calc");
        assert_eq!(shell_quote("^Test"), "'^Test'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
