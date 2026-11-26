use std::path::Path;
use std::sync::Mutex;

use futures::future::BoxFuture;

use super::*;
use crate::impact::domain::{TestOutcome, TestResult};

type Scripted = Result<TestRun, ImpactError>;

struct FakeSuite {
    on_base: bool,
    base: Mutex<Vec<Scripted>>,
    head: Mutex<Vec<Scripted>>,
    calls: Mutex<Vec<(String, Vec<TestKey>)>>,
}

impl FakeSuite {
    fn new(base: Vec<Scripted>, head: Vec<Scripted>) -> Self {
        Self { on_base: true, base: Mutex::new(base), head: Mutex::new(head), calls: Mutex::default() }
    }

    fn side(root: &Path) -> &'static str {
        if root.ends_with("base") { "base" } else { "head" }
    }
}

fn next(queue: &Mutex<Vec<Scripted>>) -> Scripted {
    let mut queue = queue.lock().unwrap();
    match queue.len() {
        0 => Err(ImpactError::Failed("no scripted run".into())),
        1 => match &queue[0] {
            Ok(run) => Ok(run.clone()),
            Err(ImpactError::NoResults(o)) => Err(ImpactError::NoResults(o.clone())),
            Err(e) => Err(ImpactError::Failed(e.to_string())),
        },
        _ => queue.remove(0),
    }
}

impl TestSuite for FakeSuite {
    fn name(&self) -> &'static str {
        "maven"
    }

    fn detect(&self, root: &Path) -> bool {
        Self::side(root) == "head" || self.on_base
    }

    fn run<'a>(&'a self, root: &'a Path, only: &'a [TestKey]) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
        let side = Self::side(root);
        self.calls.lock().unwrap().push((side.into(), only.to_vec()));
        let result = if side == "base" { next(&self.base) } else { next(&self.head) };
        Box::pin(async move { result })
    }

    fn reproduce_command(&self, _root: &Path, key: &TestKey) -> String {
        format!("mvn -Dtest={}#{} test", key.package, key.test)
    }

    fn locate(&self, _root: &Path, key: &TestKey) -> Option<(String, u32)> {
        Some((format!("src/test/java/{}.java", key.package), 7))
    }
}

struct Absent;

impl TestSuite for Absent {
    fn name(&self) -> &'static str {
        "jest"
    }
    fn detect(&self, _root: &Path) -> bool {
        false
    }
    fn run<'a>(&'a self, _: &'a Path, _: &'a [TestKey]) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
        Box::pin(async { unreachable!("an undetected suite never runs") })
    }
    fn reproduce_command(&self, _: &Path, _: &TestKey) -> String {
        String::new()
    }
    fn locate(&self, _: &Path, _: &TestKey) -> Option<(String, u32)> {
        None
    }
}

fn run(entries: &[(&str, &str, TestOutcome)]) -> TestRun {
    let mut run = TestRun::default();
    for (package, test, outcome) in entries {
        run.insert(package, test, TestResult { outcome: *outcome, output_tail: format!("{test} output") });
    }
    run
}

fn request() -> DetectRequest {
    DetectRequest {
        base_sha: "b1".into(),
        head_sha: "h1".into(),
        base_snapshot: "/w/base".into(),
        head_snapshot: "/w/head".into(),
        changed_files: vec!["src/main/java/Cart.java".into()],
        max_packages: 50,
    }
}

async fn detect(suite: Arc<FakeSuite>) -> RegressionReport {
    DetectSuiteRegressions::new(vec![suite, Arc::new(Absent)]).execute(&request()).await.unwrap()
}

#[tokio::test]
async fn nothing_detected_returns_none() {
    assert!(DetectSuiteRegressions::new(vec![Arc::new(Absent)]).execute(&request()).await.is_none());
}

#[tokio::test]
async fn base_pass_head_fail_confirmed_on_rerun_is_a_located_regression() {
    use TestOutcome::{Fail, Pass};
    let suite = Arc::new(FakeSuite::new(
        vec![Ok(run(&[("Cart", "total", Pass), ("Cart", "tax", Pass)]))],
        vec![Ok(run(&[("Cart", "total", Fail), ("Cart", "tax", Pass)])), Ok(run(&[("Cart", "total", Fail)]))],
    ));
    let report = detect(suite.clone()).await;
    assert_eq!(report.regressions.len(), 1);
    let r = &report.regressions[0];
    assert_eq!((r.package.as_str(), r.test.as_str()), ("Cart", "total"));
    assert_eq!(r.evidence.reproduce_command, "mvn -Dtest=Cart#total test");
    assert_eq!(r.evidence.head_output_tail, "total output");
    assert_eq!(r.location, Some(("src/test/java/Cart.java".into(), 7)));
    assert_eq!(report.packages_tested, vec!["maven"]);
    let calls = suite.calls.lock().unwrap();
    assert_eq!(calls.last().unwrap(), &("head".to_string(), vec![TestKey::new("Cart", "total")]));
}

#[tokio::test]
async fn flaky_preexisting_and_new_failures_are_notes_not_regressions() {
    use TestOutcome::{Fail, Pass};
    let suite = Arc::new(FakeSuite::new(
        vec![Ok(run(&[("C", "a", Pass), ("C", "old", Fail)]))],
        vec![Ok(run(&[("C", "a", Fail), ("C", "old", Fail), ("C", "new", Fail)])), Ok(run(&[("C", "a", Pass)]))],
    ));
    let report = detect(suite).await;
    assert!(report.regressions.is_empty());
    for expected in ["flaky", "pre-existing", "new tests"] {
        assert!(report.notes.iter().any(|n| n.contains(expected)), "{expected}: {:?}", report.notes);
    }
}

#[tokio::test]
async fn head_that_no_longer_produces_results_is_a_suite_level_regression() {
    use TestOutcome::Pass;
    let suite = Arc::new(FakeSuite::new(
        vec![Ok(run(&[("C", "a", Pass)]))],
        vec![Err(ImpactError::NoResults("[ERROR] COMPILATION ERROR".into()))],
    ));
    let report = detect(suite).await;
    assert_eq!(report.regressions.len(), 1);
    assert_eq!(report.regressions[0].package, "maven");
    assert_eq!(report.regressions[0].test, PACKAGE_LEVEL);
    assert!(report.regressions[0].evidence.head_output_tail.contains("COMPILATION ERROR"));
    assert!(report.notes[0].contains("build or setup broken"));
}

#[tokio::test]
async fn broken_head_that_recovers_on_retry_is_flaky() {
    use TestOutcome::Pass;
    let suite = Arc::new(FakeSuite::new(
        vec![Ok(run(&[("C", "a", Pass)]))],
        vec![Err(ImpactError::NoResults("oom".into())), Ok(run(&[("C", "a", Pass)]))],
    ));
    let report = detect(suite).await;
    assert!(report.regressions.is_empty());
    assert!(report.notes[0].contains("flaky"));
}

#[tokio::test]
async fn no_results_anywhere_is_a_note() {
    let suite = Arc::new(FakeSuite::new(vec![Ok(TestRun::default())], vec![Err(ImpactError::NoResults("x".into()))]));
    let report = detect(suite).await;
    assert!(report.regressions.is_empty());
    assert!(report.notes[0].contains("no test results on base or head"));
}

#[tokio::test]
async fn unrunnable_base_or_head_and_missing_base_suite_are_notes() {
    let failing_base = Arc::new(FakeSuite::new(vec![Err(ImpactError::Failed("mvn missing".into()))], vec![]));
    assert!(detect(failing_base).await.notes[0].contains("base commit's tests could not run"));

    let failing_head =
        Arc::new(FakeSuite::new(vec![Ok(TestRun::default())], vec![Err(ImpactError::Failed("x".into()))]));
    assert!(detect(failing_head).await.notes[0].contains("the tests could not run"));

    let mut new_suite = FakeSuite::new(vec![], vec![]);
    new_suite.on_base = false;
    let report = detect(Arc::new(new_suite)).await;
    assert!(report.notes[0].contains("does not exist on the base commit"));
}

#[tokio::test]
async fn rerun_that_cannot_run_keeps_candidates_as_notes() {
    use TestOutcome::{Fail, Pass};
    let suite = Arc::new(FakeSuite::new(
        vec![Ok(run(&[("C", "a", Pass)]))],
        vec![Ok(run(&[("C", "a", Fail)])), Err(ImpactError::Failed("timeout".into()))],
    ));
    let report = detect(suite).await;
    assert!(report.regressions.is_empty());
    assert!(report.notes.iter().any(|n| n.contains("could not be re-run")));
}

#[tokio::test]
async fn truncated_runs_are_noted() {
    use TestOutcome::Pass;
    let mut head = run(&[("C", "a", Pass)]);
    head.incomplete = true;
    let suite = Arc::new(FakeSuite::new(vec![Ok(run(&[("C", "a", Pass)]))], vec![Ok(head)]));
    assert!(detect(suite).await.notes.iter().any(|n| n.contains("incomplete")));
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

#[tokio::test]
async fn base_run_is_cached_per_suite() {
    use TestOutcome::Pass;
    let store = Arc::new(MemoryStore::default());
    let suite = Arc::new(FakeSuite::new(vec![Ok(run(&[("C", "a", Pass)]))], vec![Ok(run(&[("C", "a", Pass)]))]));
    let detect = DetectSuiteRegressions::new(vec![suite.clone()]).with_base_store(store.clone());
    detect.execute(&request()).await;
    detect.execute(&request()).await;
    assert_eq!(store.entries.lock().unwrap()[0].0.packages, vec!["suite:maven"]);
    let base_calls = suite.calls.lock().unwrap().iter().filter(|(side, _)| side == "base").count();
    assert_eq!(base_calls, 1);
}
