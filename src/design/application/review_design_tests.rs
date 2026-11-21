use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;

use super::*;
use crate::design::application::ports::CachedDesign;
use crate::design::domain::fixtures::{spec, ui_file};
use crate::review::application::DesignMismatchKind;

const LINK: &str = "https://www.figma.com/design/File1/Checkout?node-id=12-345";

struct FakeSource {
    version: Mutex<Result<String, DesignError>>,
    fetches: AtomicUsize,
    with_png: Mutex<Vec<bool>>,
}

impl FakeSource {
    fn new(version: &str) -> Arc<Self> {
        Arc::new(Self {
            version: Mutex::new(Ok(version.into())),
            fetches: AtomicUsize::new(0),
            with_png: Mutex::default(),
        })
    }
    fn set_version(&self, version: Result<String, DesignError>) {
        *self.version.lock().unwrap() = version;
    }
    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

impl FigmaSource for FakeSource {
    fn file_version<'a>(&'a self, _file_key: &'a str) -> BoxFuture<'a, Result<String, DesignError>> {
        let version = self.version.lock().unwrap().clone();
        Box::pin(async move { version })
    }
    fn fetch<'a>(&'a self, link: &'a FigmaLink, with_png: bool) -> BoxFuture<'a, Result<FetchedDesign, DesignError>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.with_png.lock().unwrap().push(with_png);
        let version = self.version.lock().unwrap().clone();
        Box::pin(async move {
            let mut design = spec(&["Pay securely now"]);
            design.node_id.clone_from(&link.node_id);
            design.variants = vec!["State=Loading".into()];
            Ok(FetchedDesign {
                spec: design,
                version: version?,
                png: with_png.then(|| vec![0x89, b'P']),
                notes: vec!["variables unavailable".into()],
            })
        })
    }
}

#[derive(Default)]
struct MemoryCache(Mutex<HashMap<FigmaLink, CachedDesign>>);

impl DesignCache for MemoryCache {
    fn get<'a>(&'a self, link: &'a FigmaLink) -> BoxFuture<'a, Result<Option<CachedDesign>, DesignError>> {
        let hit = self.0.lock().unwrap().get(link).cloned();
        Box::pin(async move { Ok(hit) })
    }
    fn put<'a>(&'a self, link: &'a FigmaLink, design: &'a FetchedDesign) -> BoxFuture<'a, Result<(), DesignError>> {
        let cached = CachedDesign {
            version: design.version.clone(),
            fetched_at: "now".into(),
            spec: design.spec.clone(),
            png: design.png.clone(),
        };
        self.0.lock().unwrap().insert(link.clone(), cached);
        Box::pin(async { Ok(()) })
    }
}

struct BrokenCache;

impl DesignCache for BrokenCache {
    fn get<'a>(&'a self, _link: &'a FigmaLink) -> BoxFuture<'a, Result<Option<CachedDesign>, DesignError>> {
        Box::pin(async { Err(DesignError::Storage("disk full".into())) })
    }
    fn put<'a>(&'a self, _link: &'a FigmaLink, _d: &'a FetchedDesign) -> BoxFuture<'a, Result<(), DesignError>> {
        Box::pin(async { Err(DesignError::Storage("disk full".into())) })
    }
}

struct FakeI18n(Vec<String>);

impl I18nSource for FakeI18n {
    fn load<'a>(&'a self, _repo_root: &'a Path) -> BoxFuture<'a, Vec<String>> {
        let texts = self.0.clone();
        Box::pin(async move { texts })
    }
}

struct FakeJudge {
    fail: bool,
    saw_png: Mutex<Option<bool>>,
}

impl StatesJudge for FakeJudge {
    fn judge<'a>(&'a self, request: StatesRequest<'a>) -> BoxFuture<'a, Result<Vec<DesignMismatch>, DesignError>> {
        *self.saw_png.lock().unwrap() = Some(request.png.is_some());
        let file = request.files[0].path.clone();
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                return Err(DesignError::Judge("model refused".into()));
            }
            Ok(vec![DesignMismatch {
                kind: DesignMismatchKind::States,
                frame: "f".into(),
                node_id: "12:345".into(),
                design_value: "State=Loading".into(),
                file,
                line: 1,
                code_evidence: "x".into(),
                explanation: "no loading state".into(),
            }])
        })
    }
}

fn use_case(
    source: Arc<FakeSource>,
    cache: Arc<dyn DesignCache>,
    judge: Option<Arc<FakeJudge>>,
    checks: &[DesignCheckKind],
) -> ReviewDesign {
    let i18n = Arc::new(FakeI18n(vec!["Something translated".into()]));
    ReviewDesign::new(source, cache, i18n, judge.map(|j| j as Arc<dyn StatesJudge>), checks.to_vec())
}

fn files() -> Vec<FileChange> {
    vec![ui_file("src/Pay.tsx", &["export const Pay = () => <b>Pay later</b>;"])]
}

async fn run(review: &ReviewDesign, files: &[FileChange], links: &[&str]) -> DesignOutcome {
    let links: Vec<String> = links.iter().map(|l| l.to_string()).collect();
    review.execute(DesignRequest { files, links: &links, repo_root: Path::new("."), context: None }).await
}

#[tokio::test]
async fn no_ui_files_means_nothing_to_do() {
    let source = FakeSource::new("v1");
    let review = use_case(source.clone(), Arc::new(MemoryCache::default()), None, &[DesignCheckKind::Copy]);
    let go = vec![ui_file("main.go", &["x := 1"])];
    assert_eq!(run(&review, &go, &[LINK]).await, DesignOutcome::default());
    assert_eq!(source.fetches(), 0);
}

#[tokio::test]
async fn missing_or_invalid_links_become_notes() {
    let review = use_case(FakeSource::new("v1"), Arc::new(MemoryCache::default()), None, &[DesignCheckKind::Copy]);
    let none = run(&review, &files(), &[]).await;
    assert!(none.mismatches.is_empty());
    assert_eq!(none.notes, vec![NO_LINKS_NOTE.to_string()]);
    let invalid = run(&review, &files(), &["https://www.figma.com/design/File1/x"]).await;
    assert_eq!(invalid.notes.len(), 1);
    assert!(invalid.notes[0].contains("whole file"), "{:?}", invalid.notes);
}

#[tokio::test]
async fn too_many_links_are_capped() {
    let links: Vec<String> = (1..=7).map(|i| format!("https://www.figma.com/design/File1/x?node-id={i}-1")).collect();
    let refs: Vec<&str> = links.iter().map(String::as_str).collect();
    let source = FakeSource::new("v1");
    let review = use_case(source.clone(), Arc::new(MemoryCache::default()), None, &[DesignCheckKind::Copy]);
    let outcome = run(&review, &files(), &refs).await;
    assert_eq!(source.fetches(), MAX_LINKS);
    assert!(outcome.notes.iter().any(|n| n.contains("first 5 of 7")));
}

#[tokio::test]
async fn reports_copy_mismatch_and_reuses_cache_until_version_changes() {
    let source = FakeSource::new("v1");
    let review = use_case(source.clone(), Arc::new(MemoryCache::default()), None, &[DesignCheckKind::Copy]);
    let first = run(&review, &files(), &[LINK, LINK]).await;
    assert_eq!(first.mismatches.len(), 1, "duplicate links are read once");
    assert_eq!(first.mismatches[0].design_value, "Pay securely now");
    assert!(first.notes.iter().any(|n| n.contains("variables unavailable")));
    assert_eq!(source.fetches(), 1);
    run(&review, &files(), &[LINK]).await;
    assert_eq!(source.fetches(), 1, "same version: served from cache");
    source.set_version(Ok("v2".into()));
    run(&review, &files(), &[LINK]).await;
    assert_eq!(source.fetches(), 2, "new version: refreshed");
}

#[tokio::test]
async fn figma_down_uses_cache_or_skips_with_note() {
    let source = FakeSource::new("v1");
    let cache = Arc::new(MemoryCache::default());
    let review = use_case(source.clone(), cache, None, &[DesignCheckKind::Copy]);
    run(&review, &files(), &[LINK]).await;
    source.set_version(Err(DesignError::Unavailable("HTTP 503".into())));
    let stale = run(&review, &files(), &[LINK]).await;
    assert_eq!(stale.mismatches.len(), 1);
    assert!(stale.notes[0].contains("using the cached version v1"), "{:?}", stale.notes);

    let empty = use_case(source, Arc::new(MemoryCache::default()), None, &[DesignCheckKind::Copy]);
    let skipped = run(&empty, &files(), &[LINK]).await;
    assert!(skipped.mismatches.is_empty());
    assert!(skipped.notes[0].contains("skipped: Figma unavailable: HTTP 503"), "{:?}", skipped.notes);
}

#[tokio::test]
async fn broken_cache_does_not_stop_the_review() {
    let source = FakeSource::new("v1");
    let review = use_case(source, Arc::new(BrokenCache), None, &[DesignCheckKind::Copy]);
    let outcome = run(&review, &files(), &[LINK]).await;
    assert_eq!(outcome.mismatches.len(), 1);
    assert_eq!(outcome.notes.iter().filter(|n| n.contains("disk full")).count(), 2);
}

#[tokio::test]
async fn states_judge_gets_the_png_and_its_errors_become_notes() {
    let source = FakeSource::new("v1");
    let judge = Arc::new(FakeJudge { fail: false, saw_png: Mutex::default() });
    let review =
        use_case(source.clone(), Arc::new(MemoryCache::default()), Some(judge.clone()), &[DesignCheckKind::States]);
    let outcome = run(&review, &files(), &[LINK]).await;
    assert_eq!(outcome.mismatches[0].kind, DesignMismatchKind::States);
    assert_eq!(*judge.saw_png.lock().unwrap(), Some(true));
    assert_eq!(*source.with_png.lock().unwrap(), vec![true]);

    let failing = Arc::new(FakeJudge { fail: true, saw_png: Mutex::default() });
    let review =
        use_case(FakeSource::new("v1"), Arc::new(MemoryCache::default()), Some(failing), &[DesignCheckKind::States]);
    let outcome = run(&review, &files(), &[LINK]).await;
    assert!(outcome.mismatches.is_empty());
    assert!(outcome.notes.iter().any(|n| n.contains("model refused")), "{:?}", outcome.notes);
}

#[tokio::test]
async fn states_without_client_is_noted_and_png_not_requested() {
    let source = FakeSource::new("v1");
    let review = use_case(source.clone(), Arc::new(MemoryCache::default()), None, &[DesignCheckKind::States]);
    let outcome = run(&review, &files(), &[LINK]).await;
    assert!(outcome.notes[0].contains("no Claude client"));
    assert_eq!(*source.with_png.lock().unwrap(), vec![false]);
}
