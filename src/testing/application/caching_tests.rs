use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use super::*;
use crate::cache::application::store::fakes::MemoryCache;
use crate::review::application::{EvidenceRef, TestEvidence};
use crate::review::domain::change_set::FileChange;
use crate::review::domain::change_set::fixtures::{added, file};

/// Object ids by path; every path not listed does not exist. Tree ids are `tree:<dir>` lookups too.
#[derive(Default)]
struct FakeIds {
    ids: Mutex<HashMap<String, String>>,
    fail: bool,
}

impl FakeIds {
    fn with(entries: &[(&str, &str)]) -> Arc<Self> {
        let ids = entries.iter().map(|(p, i)| (p.to_string(), i.to_string())).collect();
        Arc::new(Self { ids: Mutex::new(ids), fail: false })
    }

    fn set(&self, path: &str, id: &str) {
        self.ids.lock().unwrap().insert(path.into(), id.into());
    }

    fn lookup(&self, path: &str) -> Result<Option<String>, CacheError> {
        if self.fail {
            return Err(CacheError("git down".into()));
        }
        Ok(self.ids.lock().unwrap().get(path).cloned())
    }
}

impl ContentIds for FakeIds {
    fn object_id<'a>(&'a self, _rev: &'a str, path: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>> {
        Box::pin(async move { self.lookup(path) })
    }

    fn tree_id<'a>(&'a self, _rev: &'a str, dir: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>> {
        Box::pin(async move { self.lookup(&format!("tree:{dir}")) })
    }
}

/// Returns a canned result and counts calls.
struct CannedAgent {
    result: TestResult,
    calls: Mutex<u32>,
}

impl TestAgent for CannedAgent {
    fn run<'a>(&'a self, _input: &'a AgentInput<'a>, _scenario: &'a TestScenario) -> BoxFuture<'a, TestResult> {
        *self.calls.lock().unwrap() += 1;
        Box::pin(async move { self.result.clone() })
    }
}

fn scenario() -> TestScenario {
    TestScenario {
        id: "S1".into(),
        criterion_ids: vec!["SHOP-7/AC-1".into()],
        title: "sum".into(),
        given: "two numbers".into(),
        when: "Add is called".into(),
        then: "it returns 5".into(),
    }
}

fn result(verdict: TestVerdict, evidence_file: &str) -> TestResult {
    let evidence = TestEvidence {
        criterion_ids: vec!["SHOP-7/AC-1".into()],
        criterion_texts: vec!["sums".into()],
        scenario: scenario(),
        test_file: "svc/calc/qgate_s1_test.go".into(),
        test_source: "package calc".into(),
        command: vec!["go".into(), "test".into()],
        exit_code: Some(0),
        output: "ok".into(),
        code_under_test: if evidence_file.is_empty() {
            Vec::new()
        } else {
            vec![EvidenceRef { file: evidence_file.into(), line: 5, quote: "return a - b".into() }]
        },
        commit: "c1".into(),
        reproduce: "go test".into(),
    };
    TestResult {
        scenario_id: "S1".into(),
        language: "go".into(),
        verdict,
        evidence: Some(evidence),
        note: "the test passed".into(),
    }
}

fn tags() -> CacheTags {
    CacheTags { model: "m".into(), effort: "high".into(), prompt: "p1".into(), toolchain: "t1".into() }
}

fn files() -> Vec<FileChange> {
    vec![file("svc/calc/calc.go", 5, vec![added(5, "return a - b")], Some("x"))]
}

fn ids() -> Arc<FakeIds> {
    FakeIds::with(&[("svc/go.mod", "gomod1"), ("tree:svc", "svc-tree-1"), ("go.sum", "sum1")])
}

struct Harness {
    agent: CachingTestAgent,
    inner: Arc<CannedAgent>,
    cache: Arc<MemoryCache>,
}

fn harness(res: TestResult, ids: Arc<FakeIds>, tags: CacheTags, no_cache: bool, cache: Arc<MemoryCache>) -> Harness {
    let inner = Arc::new(CannedAgent { result: res, calls: Mutex::new(0) });
    let agent = CachingTestAgent::new(inner.clone(), cache.clone(), ids, tags, no_cache);
    Harness { agent, inner, cache }
}

async fn run(h: &Harness, files: &[FileChange], commit: &str) -> TestResult {
    let input = AgentInput {
        files,
        requirements: None,
        ticket: None,
        commit,
        repo_root: Path::new("/repo"),
        snapshot_root: None,
    };
    h.agent.run(&input, &scenario()).await
}

fn calls(h: &Harness) -> u32 {
    *h.inner.calls.lock().unwrap()
}

#[tokio::test]
async fn passed_result_is_reused_with_a_note_naming_the_original_commit() {
    let cache = Arc::new(MemoryCache::default());
    let first = harness(result(TestVerdict::Passed, "svc/calc/calc.go"), ids(), tags(), false, cache.clone());
    let input_files = files();
    let input = AgentInput {
        files: &input_files,
        requirements: None,
        ticket: None,
        commit: "aaa",
        repo_root: Path::new("/repo"),
        snapshot_root: None,
    };
    first.agent.run(&input, &scenario()).await;
    let second = harness(result(TestVerdict::Passed, "svc/calc/calc.go"), ids(), tags(), false, cache.clone());
    let input = AgentInput { commit: "bbb", ..input };
    let reused = second.agent.run(&input, &scenario()).await;
    assert_eq!(calls(&second), 0, "the inner agent must not run on a hit");
    assert!(reused.note.starts_with("reused from cache (commit aaa)"), "{}", reused.note);
    assert_eq!(reused.verdict, TestVerdict::Passed);
}

#[tokio::test]
async fn only_passed_results_with_covered_evidence_are_stored() {
    for res in [
        result(TestVerdict::FailedAssertion, "svc/calc/calc.go"),
        result(TestVerdict::Inconclusive, "svc/calc/calc.go"),
        result(TestVerdict::Passed, "other/outside.go"),
        result(TestVerdict::Passed, ""),
        TestResult { evidence: None, ..result(TestVerdict::Passed, "svc/calc/calc.go") },
    ] {
        let h = harness(res, ids(), tags(), false, Arc::default());
        let files = files();
        run(&h, &files, "aaa").await;
        run(&h, &files, "aaa").await;
        assert_eq!(calls(&h), 2, "uncacheable results must re-run");
        assert!(h.cache.map.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn changed_tree_model_prompt_or_manifest_is_a_miss() {
    let cache: Arc<MemoryCache> = Arc::default();
    let passed = || result(TestVerdict::Passed, "svc/calc/calc.go");
    let files = files();
    let seed = harness(passed(), ids(), tags(), false, cache.clone());
    run(&seed, &files, "aaa").await;
    assert_eq!(calls(&seed), 1);
    assert!(run(&seed, &files, "aaa").await.note.starts_with("reused"));

    let with_id = |path: &str, id: &str| {
        let i = ids();
        i.set(path, id);
        i
    };
    let variants: Vec<(CacheTags, Arc<FakeIds>)> = vec![
        (tags(), with_id("tree:svc", "svc-tree-2")),
        (tags(), with_id("go.sum", "sum2")),
        (CacheTags { model: "m2".into(), ..tags() }, ids()),
        (CacheTags { effort: "low".into(), ..tags() }, ids()),
        (CacheTags { prompt: "p2".into(), ..tags() }, ids()),
        (CacheTags { toolchain: "t2".into(), ..tags() }, ids()),
    ];
    for (tags, ids) in variants {
        let h = harness(passed(), ids, tags, false, cache.clone());
        let res = run(&h, &files, "aaa").await;
        assert_eq!(calls(&h), 1, "key input change must miss");
        assert!(!res.note.starts_with("reused"));
    }
}

#[tokio::test]
async fn no_cache_neither_reads_nor_writes() {
    let h = harness(result(TestVerdict::Passed, "svc/calc/calc.go"), ids(), tags(), true, Arc::default());
    let files = files();
    run(&h, &files, "aaa").await;
    run(&h, &files, "aaa").await;
    assert_eq!(calls(&h), 2);
    assert_eq!((*h.cache.gets.lock().unwrap(), *h.cache.puts.lock().unwrap()), (0, 0));
}

#[tokio::test]
async fn unknown_git_state_disables_caching_for_that_run() {
    let broken = Arc::new(FakeIds { fail: true, ..FakeIds::default() });
    let h = harness(result(TestVerdict::Passed, "svc/calc/calc.go"), broken, tags(), false, Arc::default());
    let files = files();
    run(&h, &files, "aaa").await;
    run(&h, &files, "aaa").await;
    assert_eq!(calls(&h), 2);
    assert!(h.cache.map.lock().unwrap().is_empty());
}

#[tokio::test]
async fn module_dir_is_the_nearest_manifest_and_falls_back_to_the_root() {
    let ids = FakeIds::with(&[("a/b/package.json", "x"), ("Cargo.toml", "y")]);
    let h = harness(result(TestVerdict::Passed, ""), ids, tags(), false, Arc::default());
    let mut memo = HashMap::new();
    assert_eq!(h.agent.module_dir("c", "a/b/c/d.ts", &mut memo).await.unwrap(), "a/b");
    assert_eq!(h.agent.module_dir("c", "a/b/e.ts", &mut memo).await.unwrap(), "a/b");
    assert_eq!(h.agent.module_dir("c", "z/q.rs", &mut memo).await.unwrap(), "");
    assert_eq!(h.agent.module_dir("c", "top.rs", &mut memo).await.unwrap(), "");
    let bare = harness(result(TestVerdict::Passed, ""), FakeIds::with(&[]), tags(), false, Arc::default());
    assert_eq!(bare.agent.module_dir("c", "m/n.rs", &mut HashMap::new()).await.unwrap(), "");
}

#[test]
fn within_requires_a_path_boundary() {
    assert!(within("svc/a.go", "svc"));
    assert!(within("anything", ""));
    assert!(!within("svcx/a.go", "svc"));
}

#[test]
fn stored_test_round_trips_through_json() {
    let stored = StoredTest { commit: "abc".into(), result: result(TestVerdict::Passed, "svc/a.go") };
    let bytes = serde_json::to_vec(&stored).unwrap();
    assert_eq!(serde_json::from_slice::<StoredTest>(&bytes).unwrap(), stored);
}
