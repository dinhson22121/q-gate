use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::ports::{BaseRunKey, BaseRunStore, GoToolchain, ImpactError, TestSelection};
use crate::impact::domain::{
    GoPackage, PACKAGE_LEVEL, Regression, RegressionReport, TestKey, TestRun, changed_packages, compare,
    compute_impacted, confirm,
};

pub const DEFAULT_MAX_PACKAGES: usize = 50;
const EVIDENCE_TAIL_BYTES: usize = 4000;

pub struct DetectRequest {
    pub base_sha: String,
    pub head_sha: String,
    pub base_snapshot: PathBuf,
    pub head_snapshot: PathBuf,
    pub changed_files: Vec<String>,
    pub max_packages: usize,
}

pub struct DetectRegressions {
    toolchain: Arc<dyn GoToolchain>,
    base_store: Option<Arc<dyn BaseRunStore>>,
}

impl DetectRegressions {
    pub fn new(toolchain: Arc<dyn GoToolchain>) -> Self {
        Self { toolchain, base_store: None }
    }

    #[must_use]
    pub fn with_base_store(mut self, store: Arc<dyn BaseRunStore>) -> Self {
        self.base_store = Some(store);
        self
    }

    pub async fn execute(&self, request: &DetectRequest) -> RegressionReport {
        match self.detect(request).await {
            Ok(report) => report,
            Err(e) => RegressionReport::skipped(e.to_string()),
        }
    }

    async fn detect(&self, request: &DetectRequest) -> Result<RegressionReport, ImpactError> {
        if !self.toolchain.is_go_module(&request.head_snapshot) {
            return Ok(RegressionReport::skipped("not a Go module"));
        }
        let head_packages = self.toolchain.list_packages(&request.head_snapshot).await?;
        let changed = changed_packages(&head_packages, &request.changed_files);
        let impacted = compute_impacted(&head_packages, &changed, request.max_packages);
        let mut report = RegressionReport { impacted: impacted.clone(), ..RegressionReport::default() };
        if impacted.selected.is_empty() {
            report.notes.push("no impacted Go package has existing tests".into());
            report.packages_skipped = impacted.impacted.clone();
            return Ok(report);
        }
        let to_run = self.packages_on_both_commits(request, &impacted.selected, &mut report).await?;
        report.packages_skipped.extend(impacted.without_tests.iter().chain(&impacted.dropped_by_cap).cloned());
        if impacted.capped() {
            report.notes.push(format!(
                "capped at {} packages: {} impacted package(s) with tests were not run",
                request.max_packages,
                impacted.dropped_by_cap.len()
            ));
        }
        let (base, head) = self.run_both(request, &to_run).await?;
        report.packages_tested = to_run;
        if base.incomplete || head.incomplete {
            report.notes.push("test output was truncated or timed out; results may be incomplete".into());
        }
        self.classify(request, &base, &head, &mut report).await?;
        Ok(report)
    }

    async fn packages_on_both_commits(
        &self,
        request: &DetectRequest,
        selected: &[String],
        report: &mut RegressionReport,
    ) -> Result<Vec<String>, ImpactError> {
        let base_packages = self.toolchain.list_packages(&request.base_snapshot).await?;
        let (existing, new): (Vec<String>, Vec<String>) =
            selected.iter().cloned().partition(|p| exists_in(&base_packages, p));
        if !new.is_empty() {
            report.notes.push(format!("{} package(s) are new on head and were not compared", new.len()));
            report.packages_skipped.extend(new);
        }
        Ok(existing)
    }

    async fn run_both(&self, request: &DetectRequest, packages: &[String]) -> Result<(TestRun, TestRun), ImpactError> {
        let selections = whole_packages(packages);
        let (base, head) = futures::join!(
            self.base_run(request, packages, &selections),
            self.toolchain.run_tests(&request.head_snapshot, &selections)
        );
        Ok((base?, head?))
    }

    pub async fn base_run(
        &self,
        request: &DetectRequest,
        packages: &[String],
        selections: &[TestSelection],
    ) -> Result<TestRun, ImpactError> {
        let mut sorted = packages.to_vec();
        sorted.sort();
        let key = BaseRunKey { base_sha: request.base_sha.clone(), packages: sorted };
        if let Some(cached) = self.base_store.as_ref().and_then(|store| store.get(&key)) {
            return Ok(cached);
        }
        let run = self.toolchain.run_tests(&request.base_snapshot, selections).await?;
        if let Some(store) = self.base_store.as_ref().filter(|_| !run.incomplete) {
            store.put(&key, &run);
        }
        Ok(run)
    }

    async fn classify(
        &self,
        request: &DetectRequest,
        base: &TestRun,
        head: &TestRun,
        report: &mut RegressionReport,
    ) -> Result<(), ImpactError> {
        let comparison = compare(base, head);
        if !comparison.pre_existing.is_empty() {
            report.notes.push(format!(
                "{} test(s) fail on base too (pre-existing, not blocking): {}",
                comparison.pre_existing.len(),
                names(&comparison.pre_existing)
            ));
        }
        if !comparison.new_failures.is_empty() {
            report.notes.push(format!(
                "{} test(s) with no passing base result fail on head (new tests; handled by the quality path): {}",
                comparison.new_failures.len(),
                names(&comparison.new_failures)
            ));
        }
        if comparison.candidates.is_empty() {
            return Ok(());
        }
        let rerun = self.toolchain.run_tests(&request.head_snapshot, &rerun_selections(&comparison.candidates)).await?;
        let (confirmed, flaky) = confirm(&comparison.candidates, &rerun);
        if !flaky.is_empty() {
            report.notes.push(format!(
                "{} test(s) failed once on head but not on re-run (flaky): {}",
                flaky.len(),
                names(&flaky)
            ));
        }
        report.regressions = confirmed.iter().map(|key| regression(key, &rerun)).collect();
        Ok(())
    }
}

fn exists_in(packages: &[GoPackage], import_path: &str) -> bool {
    packages.iter().any(|p| p.import_path == import_path)
}

fn whole_packages(packages: &[String]) -> Vec<TestSelection> {
    packages.iter().map(|package| TestSelection { package: package.clone(), tests: Vec::new() }).collect()
}

fn rerun_selections(candidates: &[TestKey]) -> Vec<TestSelection> {
    let mut by_package: BTreeMap<&str, Option<Vec<String>>> = BTreeMap::new();
    for key in candidates {
        let entry = by_package.entry(key.package.as_str()).or_insert_with(|| Some(Vec::new()));
        if key.test == PACKAGE_LEVEL {
            *entry = None;
        } else if let Some(tests) = entry {
            let top = key.test.split('/').next().unwrap_or_default().to_string();
            if !tests.contains(&top) {
                tests.push(top);
            }
        }
    }
    by_package
        .into_iter()
        .map(|(package, tests)| TestSelection { package: package.to_string(), tests: tests.unwrap_or_default() })
        .collect()
}

fn regression(key: &TestKey, rerun: &TestRun) -> Regression {
    let output = rerun.result(&key.package, &key.test).map(|r| r.output_tail.as_str()).unwrap_or_default();
    Regression::new(&key.package, &key.test, tail(output, EVIDENCE_TAIL_BYTES))
}

fn tail(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
}

fn names(keys: &[TestKey]) -> String {
    let mut shown: Vec<String> = keys.iter().take(10).map(|k| format!("{}.{}", k.package, k.test)).collect();
    if keys.len() > shown.len() {
        shown.push(format!("... +{} more", keys.len() - shown.len()));
    }
    shown.join(", ")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use futures::future::BoxFuture;

    use super::*;
    use crate::impact::domain::{TestOutcome, TestResult};

    fn pkg(name: &str, imports: &[&str], has_tests: bool) -> GoPackage {
        GoPackage {
            import_path: name.into(),
            rel_dir: name.strip_prefix("m/").unwrap_or("").into(),
            imports: imports.iter().map(|s| s.to_string()).collect(),
            has_tests,
        }
    }

    fn run(entries: &[(&str, &str, TestOutcome)]) -> TestRun {
        let mut run = TestRun::default();
        for (package, test, outcome) in entries {
            run.insert(package, test, TestResult { outcome: *outcome, output_tail: format!("{test} output") });
        }
        run
    }

    struct FakeGo {
        module: bool,
        base_packages: Result<Vec<GoPackage>, String>,
        head_packages: Result<Vec<GoPackage>, String>,
        base_run: TestRun,
        head_runs: Mutex<Vec<TestRun>>,
        calls: Mutex<Vec<(String, Vec<TestSelection>)>>,
    }

    impl FakeGo {
        fn new(packages: Vec<GoPackage>, base_run: TestRun, head_runs: Vec<TestRun>) -> Self {
            Self {
                module: true,
                base_packages: Ok(packages.clone()),
                head_packages: Ok(packages),
                base_run,
                head_runs: Mutex::new(head_runs),
                calls: Mutex::default(),
            }
        }
    }

    impl GoToolchain for FakeGo {
        fn is_go_module(&self, _root: &Path) -> bool {
            self.module
        }

        fn list_packages<'a>(&'a self, root: &'a Path) -> BoxFuture<'a, Result<Vec<GoPackage>, ImpactError>> {
            let listed = if root.ends_with("base") { &self.base_packages } else { &self.head_packages };
            let result = listed.clone().map_err(ImpactError::ToolchainUnavailable);
            Box::pin(async move { result })
        }

        fn run_tests<'a>(
            &'a self,
            root: &'a Path,
            selections: &'a [TestSelection],
        ) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
            let side = if root.ends_with("base") { "base" } else { "head" };
            self.calls.lock().unwrap().push((side.into(), selections.to_vec()));
            let run = if side == "base" {
                self.base_run.clone()
            } else {
                let mut runs = self.head_runs.lock().unwrap();
                if runs.len() > 1 { runs.remove(0) } else { runs[0].clone() }
            };
            Box::pin(async move { Ok(run) })
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        entries: Mutex<Vec<(BaseRunKey, TestRun)>>,
    }

    impl BaseRunStore for MemoryStore {
        fn get(&self, key: &BaseRunKey) -> Option<TestRun> {
            self.entries.lock().unwrap().iter().find(|(k, _)| k == key).map(|(_, r)| r.clone())
        }

        fn put(&self, key: &BaseRunKey, run: &TestRun) {
            self.entries.lock().unwrap().push((key.clone(), run.clone()));
        }
    }

    fn request(changed: &[&str], max: usize) -> DetectRequest {
        DetectRequest {
            base_sha: "base1".into(),
            head_sha: "head1".into(),
            base_snapshot: "/w/base".into(),
            head_snapshot: "/w/head".into(),
            changed_files: changed.iter().map(|s| s.to_string()).collect(),
            max_packages: max,
        }
    }

    fn graph() -> Vec<GoPackage> {
        vec![pkg("m/api", &["m/util"], true), pkg("m/util", &[], true), pkg("m/doc", &[], false)]
    }

    #[tokio::test]
    async fn confirmed_regression_is_reported_with_evidence() {
        use TestOutcome::{Fail, Pass};
        let base = run(&[("m/api", "TestA", Pass), ("m/util", "TestU", Pass)]);
        let head = run(&[("m/api", "TestA", Fail), ("m/util", "TestU", Pass)]);
        let go = Arc::new(FakeGo::new(graph(), base, vec![head]));
        let report = DetectRegressions::new(go.clone()).execute(&request(&["util/u.go"], 50)).await;

        assert!(report.blocks());
        let r = &report.regressions[0];
        assert_eq!((r.package.as_str(), r.test.as_str()), ("m/api", "TestA"));
        assert_eq!(r.evidence.head_output_tail, "TestA output");
        assert_eq!(r.evidence.base_outcome, TestOutcome::Pass);
        assert_eq!(r.evidence.reproduce_command, "go test -count=1 -run '^TestA$' m/api");
        assert_eq!(report.packages_tested, vec!["m/util", "m/api"]);
        let calls = go.calls.lock().unwrap();
        let rerun = calls.last().unwrap();
        assert_eq!(rerun.0, "head");
        assert_eq!(rerun.1, vec![TestSelection { package: "m/api".into(), tests: vec!["TestA".into()] }]);
    }

    #[tokio::test]
    async fn flaky_failure_that_passes_on_rerun_is_not_a_regression() {
        use TestOutcome::{Fail, Pass};
        let base = run(&[("m/api", "TestA", Pass)]);
        let go = Arc::new(FakeGo::new(
            graph(),
            base,
            vec![run(&[("m/api", "TestA", Fail)]), run(&[("m/api", "TestA", Pass)])],
        ));
        let report = DetectRegressions::new(go).execute(&request(&["api/a.go"], 50)).await;
        assert!(!report.blocks());
        assert!(report.notes.iter().any(|n| n.contains("flaky")));
    }

    #[tokio::test]
    async fn preexisting_and_new_test_failures_do_not_block_but_are_noted() {
        use TestOutcome::{Fail, Pass};
        let base = run(&[("m/api", "TestOld", Fail), ("m/api", "TestOk", Pass)]);
        let head = run(&[("m/api", "TestOld", Fail), ("m/api", "TestNew", Fail), ("m/api", "TestOk", Pass)]);
        let go = Arc::new(FakeGo::new(graph(), base, vec![head]));
        let report = DetectRegressions::new(go.clone()).execute(&request(&["api/a.go"], 50)).await;
        assert!(!report.blocks());
        assert!(report.notes.iter().any(|n| n.contains("pre-existing")));
        assert!(report.notes.iter().any(|n| n.contains("new tests")));
        assert_eq!(go.calls.lock().unwrap().len(), 2, "no re-run without candidates");
    }

    #[tokio::test]
    async fn package_level_failure_reruns_the_whole_package() {
        use TestOutcome::{Fail, Pass};
        let base = run(&[("m/api", PACKAGE_LEVEL, Pass)]);
        let head = run(&[("m/api", PACKAGE_LEVEL, Fail)]);
        let go = Arc::new(FakeGo::new(graph(), base, vec![head]));
        let report = DetectRegressions::new(go.clone()).execute(&request(&["api/a.go"], 50)).await;
        assert_eq!(report.regressions[0].evidence.reproduce_command, "go test -count=1 m/api");
        assert!(go.calls.lock().unwrap().last().unwrap().1[0].tests.is_empty());
    }

    #[tokio::test]
    async fn cap_limits_packages_and_is_noted() {
        let go = Arc::new(FakeGo::new(graph(), TestRun::default(), vec![TestRun::default()]));
        let report = DetectRegressions::new(go.clone()).execute(&request(&["util/u.go"], 1)).await;
        assert_eq!(report.packages_tested, vec!["m/util"]);
        assert!(report.packages_skipped.contains(&"m/api".to_string()));
        assert!(report.notes.iter().any(|n| n.contains("capped at 1")));
    }

    #[tokio::test]
    async fn packages_without_tests_or_new_on_head_are_skipped() {
        let mut go = FakeGo::new(graph(), TestRun::default(), vec![TestRun::default()]);
        go.base_packages = Ok(vec![pkg("m/util", &[], true)]);
        let report = DetectRegressions::new(Arc::new(go)).execute(&request(&["util/u.go", "doc/d.go"], 50)).await;
        assert_eq!(report.packages_tested, vec!["m/util"]);
        assert!(report.packages_skipped.contains(&"m/doc".to_string()));
        assert!(report.packages_skipped.contains(&"m/api".to_string()));
        assert!(report.notes.iter().any(|n| n.contains("new on head")));
    }

    #[tokio::test]
    async fn nothing_to_test_yields_a_note_not_a_skip() {
        let go = Arc::new(FakeGo::new(graph(), TestRun::default(), vec![TestRun::default()]));
        let report = DetectRegressions::new(go.clone()).execute(&request(&["README.md"], 50)).await;
        assert!(report.skipped_reason.is_none() && !report.blocks());
        assert!(report.notes[0].contains("no impacted"));
        assert!(go.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn not_a_go_module_and_toolchain_errors_skip() {
        let mut go = FakeGo::new(graph(), TestRun::default(), vec![TestRun::default()]);
        go.module = false;
        let report = DetectRegressions::new(Arc::new(go)).execute(&request(&["a.go"], 5)).await;
        assert_eq!(report.skipped_reason.as_deref(), Some("not a Go module"));

        let mut go = FakeGo::new(graph(), TestRun::default(), vec![TestRun::default()]);
        go.head_packages = Err("no go".into());
        let report = DetectRegressions::new(Arc::new(go)).execute(&request(&["a.go"], 5)).await;
        assert!(report.skipped_reason.unwrap().contains("no go"));

        let mut go = FakeGo::new(graph(), TestRun::default(), vec![TestRun::default()]);
        go.base_packages = Err("base broken".into());
        let report = DetectRegressions::new(Arc::new(go)).execute(&request(&["util/u.go"], 5)).await;
        assert!(report.skipped_reason.unwrap().contains("base broken"));
    }

    #[tokio::test]
    async fn base_results_come_from_the_store_when_present_and_are_stored_when_complete() {
        use TestOutcome::Pass;
        let store = Arc::new(MemoryStore::default());
        let base = run(&[("m/util", "TestU", Pass)]);
        let go = Arc::new(FakeGo::new(graph(), base.clone(), vec![base.clone()]));
        let detect = DetectRegressions::new(go.clone()).with_base_store(store.clone());
        detect.execute(&request(&["util/u.go"], 1)).await;
        assert_eq!(store.entries.lock().unwrap().len(), 1);
        assert_eq!(store.entries.lock().unwrap()[0].0.content_inputs()[0].1, "base1");
        detect.execute(&request(&["util/u.go"], 1)).await;
        let base_runs = go.calls.lock().unwrap().iter().filter(|(side, _)| side == "base").count();
        assert_eq!(base_runs, 1, "second run is served from the store");
    }

    #[tokio::test]
    async fn incomplete_base_runs_are_not_stored_and_are_noted() {
        let store = Arc::new(MemoryStore::default());
        let base = TestRun { incomplete: true, ..TestRun::default() };
        let go = Arc::new(FakeGo::new(graph(), base, vec![TestRun::default()]));
        let report =
            DetectRegressions::new(go).with_base_store(store.clone()).execute(&request(&["util/u.go"], 1)).await;
        assert!(store.entries.lock().unwrap().is_empty());
        assert!(report.notes.iter().any(|n| n.contains("incomplete")));
    }

    #[test]
    fn rerun_selection_groups_top_level_names() {
        let keys = vec![
            TestKey::new("a", "TestX/one"),
            TestKey::new("a", "TestX/two"),
            TestKey::new("a", "TestY"),
            TestKey::new("b", PACKAGE_LEVEL),
            TestKey::new("b", "TestZ"),
        ];
        let selections = rerun_selections(&keys);
        assert_eq!(selections[0].tests, vec!["TestX", "TestY"]);
        assert_eq!(selections[1], TestSelection { package: "b".into(), tests: vec![] });
    }

    #[test]
    fn evidence_tail_keeps_the_end_on_a_char_boundary() {
        assert_eq!(tail("short", 100), "short");
        assert_eq!(tail("ééééé", 5), "...éé");
    }

    #[test]
    fn names_are_truncated() {
        let keys: Vec<TestKey> = (0..12).map(|i| TestKey::new("p", &format!("T{i}"))).collect();
        assert!(names(&keys).ends_with("... +2 more"));
    }
}
