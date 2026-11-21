use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::review::application::{Category, Confidence, DesignMismatchKind, GatePolicy, Severity};

#[derive(Debug, Clone, PartialEq)]
pub struct RunFacts {
    pub run_id: i64,
    pub repo_path: String,
    pub repo_name: String,
    pub created_at: DateTime<Utc>,
    pub recorded_passed: bool,
    pub findings: Vec<(Severity, Category, Confidence)>,
    pub unmet_criteria: usize,
    pub failed_checks: usize,
    pub incomplete: bool,
    pub failed_tests: usize,
    pub rule_violations: usize,
    pub design_mismatches: Vec<DesignMismatchKind>,
}

pub fn passes(policy: &GatePolicy, run: &RunFacts) -> bool {
    let blocking = run.findings.iter().any(|(severity, category, confidence)| {
        *severity >= policy.fail_on && *confidence >= policy.min_confidence && policy.categories.contains(category)
    });
    let design_blocks = run
        .design_mismatches
        .iter()
        .any(|kind| *kind != DesignMismatchKind::States && policy.fail_on_design.contains(kind));
    !blocking
        && run.failed_checks == 0
        && !(policy.fail_on_unmet_criteria && run.unmet_criteria > 0)
        && !(policy.fail_on_incomplete && run.incomplete)
        && !(policy.fail_on_test_failure && run.failed_tests > 0)
        && !(policy.fail_on_rule_violation && run.rule_violations > 0)
        && !design_blocks
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRunRow {
    pub run_id: i64,
    pub repo_name: String,
    pub created_at: DateTime<Utc>,
    pub recorded_passed: bool,
    pub before_passed: bool,
    pub after_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRunReport {
    pub evaluated: usize,
    pub skipped: usize,
    pub pass_to_fail: usize,
    pub fail_to_pass: usize,
    pub flips: Vec<DryRunRow>,
    pub drifted: usize,
    pub caveats: Vec<String>,
}

pub const CAVEAT_MODEL_RESULTS: &str = "Findings, rule violations and generated-test verdicts are replayed as recorded; \
     only the deterministic gate is re-evaluated.";
pub const CAVEAT_NEW_RULES: &str = "New or edited rules are judged by the model on future commits and cannot be \
     replayed on history: their effect is not evaluated here (estimate only).";

pub fn summarize(
    runs: &[RunFacts],
    skipped: usize,
    has_rule_changes: bool,
    before: impl Fn(&RunFacts) -> GatePolicy,
    after: impl Fn(&RunFacts) -> GatePolicy,
) -> DryRunReport {
    let mut report = DryRunReport {
        evaluated: runs.len(),
        skipped,
        pass_to_fail: 0,
        fail_to_pass: 0,
        flips: Vec::new(),
        drifted: 0,
        caveats: vec![CAVEAT_MODEL_RESULTS.to_string()],
    };
    if has_rule_changes {
        report.caveats.push(CAVEAT_NEW_RULES.to_string());
    }
    for run in runs {
        let (before_passed, after_passed) = (passes(&before(run), run), passes(&after(run), run));
        if before_passed != run.recorded_passed {
            report.drifted += 1;
        }
        if before_passed == after_passed {
            continue;
        }
        if before_passed {
            report.pass_to_fail += 1
        } else {
            report.fail_to_pass += 1
        }
        report.flips.push(DryRunRow {
            run_id: run.run_id,
            repo_name: run.repo_name.clone(),
            created_at: run.created_at,
            recorded_passed: run.recorded_passed,
            before_passed,
            after_passed,
        });
    }
    report
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn run(id: i64, repo: &str) -> RunFacts {
        RunFacts {
            run_id: id,
            repo_path: format!("/repos/{repo}"),
            repo_name: repo.into(),
            created_at: DateTime::parse_from_rfc3339("2026-11-20T10:00:00Z").unwrap().with_timezone(&Utc),
            recorded_passed: true,
            findings: Vec::new(),
            unmet_criteria: 0,
            failed_checks: 0,
            incomplete: false,
            failed_tests: 0,
            rule_violations: 0,
            design_mismatches: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::run;
    use super::*;

    #[test]
    fn replays_every_gate_input() {
        let policy = GatePolicy::default();
        assert!(passes(&policy, &run(1, "a")));
        let major =
            RunFacts { findings: vec![(Severity::Major, Category::Correctness, Confidence::High)], ..run(1, "a") };
        assert!(passes(&policy, &major));
        assert!(!passes(&GatePolicy { fail_on: Severity::Major, ..policy.clone() }, &major));
        let unsure =
            RunFacts { findings: vec![(Severity::Critical, Category::Security, Confidence::Low)], ..run(1, "a") };
        assert!(passes(&policy, &unsure));
        assert!(!passes(&policy, &RunFacts { failed_checks: 1, ..run(1, "a") }));
        let unmet = RunFacts { unmet_criteria: 1, ..run(1, "a") };
        assert!(!passes(&policy, &unmet));
        assert!(passes(&GatePolicy { fail_on_unmet_criteria: false, ..policy.clone() }, &unmet));
        let incomplete = RunFacts { incomplete: true, ..run(1, "a") };
        assert!(!passes(&policy, &incomplete));
        assert!(passes(&GatePolicy { fail_on_incomplete: false, ..policy.clone() }, &incomplete));
        let agents = RunFacts {
            failed_tests: 1,
            rule_violations: 1,
            design_mismatches: vec![DesignMismatchKind::States, DesignMismatchKind::Copy],
            ..run(1, "a")
        };
        assert!(passes(&policy, &agents), "agent results only block when the server says so");
        assert!(!passes(&GatePolicy { fail_on_test_failure: true, ..policy.clone() }, &agents));
        assert!(!passes(&GatePolicy { fail_on_rule_violation: true, ..policy.clone() }, &agents));
        assert!(!passes(&GatePolicy { fail_on_design: vec![DesignMismatchKind::Copy], ..policy.clone() }, &agents));
        let states_only = RunFacts { design_mismatches: vec![DesignMismatchKind::States], ..run(1, "a") };
        assert!(passes(&GatePolicy { fail_on_design: vec![DesignMismatchKind::States], ..policy }, &states_only));
    }

    #[test]
    fn counts_flips_both_ways_and_drift() {
        let failing_test = RunFacts { failed_tests: 1, ..run(1, "a") };
        let incomplete = RunFacts { incomplete: true, recorded_passed: false, ..run(2, "b") };
        let drifted = RunFacts { unmet_criteria: 1, recorded_passed: true, ..run(3, "c") };
        let runs = vec![failing_test, incomplete, drifted];
        let before = |_: &RunFacts| GatePolicy::default();
        let after = |r: &RunFacts| GatePolicy {
            fail_on_test_failure: true,
            fail_on_incomplete: r.repo_name != "b",
            ..GatePolicy::default()
        };
        let report = summarize(&runs, 2, true, before, after);
        assert_eq!((report.evaluated, report.skipped), (3, 2));
        assert_eq!((report.pass_to_fail, report.fail_to_pass), (1, 1));
        assert_eq!(report.flips.iter().map(|f| f.run_id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(report.flips[0].before_passed && !report.flips[0].after_passed);
        assert_eq!(report.drifted, 1);
        assert_eq!(report.caveats.len(), 2);
        let quiet = summarize(&runs, 0, false, before, before);
        assert!(quiet.flips.is_empty());
        assert_eq!(quiet.caveats, vec![CAVEAT_MODEL_RESULTS.to_string()]);
    }
}
