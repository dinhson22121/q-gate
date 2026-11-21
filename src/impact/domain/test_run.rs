use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PACKAGE_LEVEL: &str = "";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    pub outcome: TestOutcome,
    pub output_tail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    pub packages: BTreeMap<String, BTreeMap<String, TestResult>>,
    #[serde(default)]
    pub incomplete: bool,
}

impl TestRun {
    pub fn insert(&mut self, package: &str, test: &str, result: TestResult) {
        self.packages.entry(package.to_string()).or_default().insert(test.to_string(), result);
    }

    pub fn result(&self, package: &str, test: &str) -> Option<&TestResult> {
        self.packages.get(package)?.get(test)
    }

    pub fn outcome(&self, package: &str, test: &str) -> Option<TestOutcome> {
        self.result(package, test).map(|r| r.outcome)
    }

    pub fn failures(&self) -> impl Iterator<Item = (&str, &str, &TestResult)> {
        self.packages.iter().flat_map(|(package, tests)| {
            tests
                .iter()
                .filter(|(_, r)| r.outcome == TestOutcome::Fail)
                .map(move |(test, r)| (package.as_str(), test.as_str(), r))
        })
    }

    pub fn merge(&mut self, other: TestRun) {
        self.incomplete |= other.incomplete;
        for (package, tests) in other.packages {
            self.packages.entry(package).or_default().extend(tests);
        }
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn result(outcome: TestOutcome) -> TestResult {
        TestResult { outcome, output_tail: format!("{outcome:?} output") }
    }

    pub fn run(entries: &[(&str, &str, TestOutcome)]) -> TestRun {
        let mut run = TestRun::default();
        for (package, test, outcome) in entries {
            run.insert(package, test, result(*outcome));
        }
        run
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::run;
    use super::*;

    #[test]
    fn lookup_failures_and_merge() {
        let mut a = run(&[("p", "TestA", TestOutcome::Pass), ("p", "TestB", TestOutcome::Fail)]);
        let mut b = run(&[("q", "TestC", TestOutcome::Fail)]);
        b.incomplete = true;
        assert_eq!(a.outcome("p", "TestA"), Some(TestOutcome::Pass));
        assert_eq!(a.outcome("p", "Nope"), None);
        assert_eq!(a.failures().map(|(p, t, _)| (p, t)).collect::<Vec<_>>(), vec![("p", "TestB")]);
        a.merge(b);
        assert!(a.incomplete);
        assert_eq!(a.failures().count(), 2);
    }

    #[test]
    fn serde_round_trip() {
        let a = run(&[("p", PACKAGE_LEVEL, TestOutcome::Skip)]);
        let json = serde_json::to_vec(&a).unwrap();
        assert_eq!(serde_json::from_slice::<TestRun>(&json).unwrap(), a);
    }
}
