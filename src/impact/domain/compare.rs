//! Regression = passes on base, fails on head, and fails again when re-run on head.

use std::collections::BTreeSet;

use super::test_run::{PACKAGE_LEVEL, TestOutcome, TestRun};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TestKey {
    pub package: String,
    pub test: String,
}

impl TestKey {
    pub fn new(package: &str, test: &str) -> Self {
        Self { package: package.to_string(), test: test.to_string() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Passed on base, failed on head: regressions pending a re-run.
    pub candidates: Vec<TestKey>,
    /// Failed on base too: not caused by this change.
    pub pre_existing: Vec<TestKey>,
    /// Failed on head with no passing base result (new tests): the quality path's concern.
    pub new_failures: Vec<TestKey>,
}

pub fn compare(base: &TestRun, head: &TestRun) -> Comparison {
    let mut out = Comparison::default();
    for (package, test, _) in head.failures() {
        if test == PACKAGE_LEVEL && has_named_failure(head, package) {
            continue;
        }
        let key = TestKey::new(package, test);
        match base.outcome(package, test) {
            Some(TestOutcome::Pass) => out.candidates.push(key),
            Some(TestOutcome::Fail) => out.pre_existing.push(key),
            Some(TestOutcome::Skip) | None => out.new_failures.push(key),
        }
    }
    out.candidates = without_failed_parents(out.candidates);
    out
}

fn has_named_failure(run: &TestRun, package: &str) -> bool {
    run.failures().any(|(p, t, _)| p == package && t != PACKAGE_LEVEL)
}

/// A failing subtest fails its parent too; report only the leaf.
fn without_failed_parents(keys: Vec<TestKey>) -> Vec<TestKey> {
    let all: BTreeSet<&TestKey> = keys.iter().collect();
    let parents: BTreeSet<TestKey> = all
        .iter()
        .flat_map(|k| {
            k.test
                .match_indices('/')
                .map(|(i, _)| TestKey { package: k.package.clone(), test: k.test[..i].to_string() })
        })
        .collect();
    keys.into_iter().filter(|k| !parents.contains(k)).collect()
}

/// Splits candidates into `(confirmed, flaky)` using a re-run on head: confirmed only if the same
/// test fails again; a test that passes, or is missing from the re-run, is treated as flaky.
pub fn confirm(candidates: &[TestKey], rerun: &TestRun) -> (Vec<TestKey>, Vec<TestKey>) {
    candidates.iter().cloned().partition(|k| rerun.outcome(&k.package, &k.test) == Some(TestOutcome::Fail))
}

#[cfg(test)]
mod tests {
    use super::super::test_run::fixtures::run;
    use super::*;
    use TestOutcome::{Fail, Pass, Skip};

    #[test]
    fn base_pass_head_fail_is_a_candidate() {
        let base = run(&[("p", "TestA", Pass)]);
        let head = run(&[("p", "TestA", Fail)]);
        let c = compare(&base, &head);
        assert_eq!(c.candidates, vec![TestKey::new("p", "TestA")]);
        assert!(c.pre_existing.is_empty() && c.new_failures.is_empty());
    }

    #[test]
    fn failing_on_both_is_pre_existing() {
        let c = compare(&run(&[("p", "TestA", Fail)]), &run(&[("p", "TestA", Fail)]));
        assert!(c.candidates.is_empty());
        assert_eq!(c.pre_existing, vec![TestKey::new("p", "TestA")]);
    }

    #[test]
    fn new_or_previously_skipped_test_failing_is_not_a_regression() {
        let base = run(&[("p", "TestS", Skip)]);
        let head = run(&[("p", "TestS", Fail), ("p", "TestNew", Fail)]);
        let c = compare(&base, &head);
        assert!(c.candidates.is_empty());
        assert_eq!(c.new_failures.len(), 2);
    }

    #[test]
    fn head_pass_or_removed_test_is_not_flagged() {
        let base = run(&[("p", "TestA", Pass), ("p", "TestGone", Pass)]);
        let head = run(&[("p", "TestA", Pass)]);
        assert_eq!(compare(&base, &head), Comparison::default());
    }

    #[test]
    fn only_the_failing_leaf_subtest_is_reported() {
        let base = run(&[("p", "TestA", Pass), ("p", "TestA/x", Pass), ("p", "TestA/y", Pass)]);
        let head = run(&[("p", "TestA", Fail), ("p", "TestA/x", Fail), ("p", "TestA/y", Pass)]);
        assert_eq!(compare(&base, &head).candidates, vec![TestKey::new("p", "TestA/x")]);
    }

    #[test]
    fn package_level_failure_counts_only_without_named_failures() {
        let base = run(&[("p", PACKAGE_LEVEL, Pass), ("p", "TestA", Pass), ("q", PACKAGE_LEVEL, Pass)]);
        let head = run(&[("p", PACKAGE_LEVEL, Fail), ("p", "TestA", Fail), ("q", PACKAGE_LEVEL, Fail)]);
        let c = compare(&base, &head);
        assert_eq!(c.candidates, vec![TestKey::new("p", "TestA"), TestKey::new("q", PACKAGE_LEVEL)]);
    }

    #[test]
    fn confirm_keeps_repeat_failures_and_drops_flaky_ones() {
        let candidates = vec![TestKey::new("p", "TestA"), TestKey::new("p", "TestB"), TestKey::new("p", "TestC")];
        let rerun = run(&[("p", "TestA", Fail), ("p", "TestB", Pass)]);
        let (confirmed, flaky) = confirm(&candidates, &rerun);
        assert_eq!(confirmed, vec![TestKey::new("p", "TestA")]);
        assert_eq!(flaky, vec![TestKey::new("p", "TestB"), TestKey::new("p", "TestC")]);
    }
}
