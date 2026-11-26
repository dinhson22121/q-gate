use std::sync::Arc;

use super::detect::{DetectRequest, EVIDENCE_TAIL_BYTES, names, tail};
use super::ports::{BaseRunKey, BaseRunStore, ImpactError, TestSuite};
use crate::impact::domain::{PACKAGE_LEVEL, Regression, RegressionReport, TestKey, TestRun, compare, confirm};

pub struct DetectSuiteRegressions {
    suites: Vec<Arc<dyn TestSuite>>,
    base_store: Option<Arc<dyn BaseRunStore>>,
}

impl DetectSuiteRegressions {
    pub fn new(suites: Vec<Arc<dyn TestSuite>>) -> Self {
        Self { suites, base_store: None }
    }

    #[must_use]
    pub fn with_base_store(mut self, store: Arc<dyn BaseRunStore>) -> Self {
        self.base_store = Some(store);
        self
    }

    pub async fn execute(&self, request: &DetectRequest) -> Option<RegressionReport> {
        let detected: Vec<&Arc<dyn TestSuite>> =
            self.suites.iter().filter(|s| s.detect(&request.head_snapshot)).collect();
        if detected.is_empty() {
            return None;
        }
        let mut report = RegressionReport::default();
        for suite in detected {
            report.merge(self.run_suite(suite.as_ref(), request).await);
        }
        Some(report)
    }

    async fn run_suite(&self, suite: &dyn TestSuite, request: &DetectRequest) -> RegressionReport {
        let name = suite.name();
        if !suite.detect(&request.base_snapshot) {
            return note_only(format!("{name}: the suite does not exist on the base commit, so nothing was compared"));
        }
        let base = match self.base_run(suite, request).await {
            Ok(run) => run,
            Err(e) => {
                return note_only(format!(
                    "{name}: regression check skipped, the base commit's tests could not run: {e}"
                ));
            }
        };
        let head = match suite.run(&request.head_snapshot, &[]).await {
            Ok(run) => run,
            Err(ImpactError::NoResults(output)) => return self.broken_head(suite, request, &base, output).await,
            Err(e) => return note_only(format!("{name}: regression check skipped, the tests could not run: {e}")),
        };
        let mut report = RegressionReport { packages_tested: vec![name.to_string()], ..RegressionReport::default() };
        if base.incomplete || head.incomplete {
            report.notes.push(format!("{name}: test output was truncated or timed out; results may be incomplete"));
        }
        self.classify(suite, request, &base, &head, &mut report).await;
        report
    }

    async fn base_run(&self, suite: &dyn TestSuite, request: &DetectRequest) -> Result<TestRun, ImpactError> {
        let key = BaseRunKey { base_sha: request.base_sha.clone(), packages: vec![format!("suite:{}", suite.name())] };
        if let Some(cached) = self.base_store.as_ref().and_then(|store| store.get(&key)) {
            return Ok(cached);
        }
        let run = suite.run(&request.base_snapshot, &[]).await?;
        if let Some(store) = self.base_store.as_ref().filter(|_| !run.incomplete) {
            store.put(&key, &run);
        }
        Ok(run)
    }

    async fn broken_head(
        &self,
        suite: &dyn TestSuite,
        request: &DetectRequest,
        base: &TestRun,
        output: String,
    ) -> RegressionReport {
        let name = suite.name();
        let passed_on_base = base.packages.values().flat_map(|tests| tests.values()).count() > 0;
        if !passed_on_base {
            return note_only(format!("{name}: no test results on base or head, so nothing was compared"));
        }
        if suite.run(&request.head_snapshot, &[]).await.is_ok() {
            return note_only(format!("{name}: the suite failed to run once on head but ran on retry (flaky)"));
        }
        let key = TestKey::new(name, PACKAGE_LEVEL);
        let regression = Regression::with_command(
            name,
            PACKAGE_LEVEL,
            tail(&output, EVIDENCE_TAIL_BYTES),
            suite.reproduce_command(&request.head_snapshot, &key),
        );
        RegressionReport {
            regressions: vec![regression],
            notes: vec![format!(
                "{name}: the tests ran on base but produce no results on head (build or setup broken)"
            )],
            packages_tested: vec![name.to_string()],
            ..RegressionReport::default()
        }
    }

    async fn classify(
        &self,
        suite: &dyn TestSuite,
        request: &DetectRequest,
        base: &TestRun,
        head: &TestRun,
        report: &mut RegressionReport,
    ) {
        let name = suite.name();
        let comparison = compare(base, head);
        if !comparison.pre_existing.is_empty() {
            report.notes.push(format!(
                "{name}: {} test(s) fail on base too (pre-existing, not blocking): {}",
                comparison.pre_existing.len(),
                names(&comparison.pre_existing)
            ));
        }
        if !comparison.new_failures.is_empty() {
            report.notes.push(format!(
                "{name}: {} test(s) with no passing base result fail on head (new tests, not a regression): {}",
                comparison.new_failures.len(),
                names(&comparison.new_failures)
            ));
        }
        if comparison.candidates.is_empty() {
            return;
        }
        let rerun = match suite.run(&request.head_snapshot, &comparison.candidates).await {
            Ok(run) => run,
            Err(e) => {
                report.notes.push(format!(
                    "{name}: {} failing test(s) could not be re-run to confirm, so they are reported as notes only: {} ({e})",
                    comparison.candidates.len(),
                    names(&comparison.candidates)
                ));
                return;
            }
        };
        let (confirmed, flaky) = confirm(&comparison.candidates, &rerun);
        if !flaky.is_empty() {
            report.notes.push(format!(
                "{name}: {} test(s) failed once on head but not on re-run (flaky): {}",
                flaky.len(),
                names(&flaky)
            ));
        }
        report.regressions = confirmed.iter().map(|key| regression(suite, request, key, &rerun)).collect();
    }
}

fn regression(suite: &dyn TestSuite, request: &DetectRequest, key: &TestKey, rerun: &TestRun) -> Regression {
    let output = rerun.result(&key.package, &key.test).map(|r| r.output_tail.as_str()).unwrap_or_default();
    Regression::with_command(
        &key.package,
        &key.test,
        tail(output, EVIDENCE_TAIL_BYTES),
        suite.reproduce_command(&request.head_snapshot, key),
    )
    .at(suite.locate(&request.head_snapshot, key))
}

fn note_only(note: String) -> RegressionReport {
    RegressionReport { notes: vec![note], ..RegressionReport::default() }
}

#[cfg(test)]
#[path = "detect_suites_tests.rs"]
mod tests;
