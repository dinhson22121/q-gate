//! `WatchCommits` driven through fake ports, in the push-triggered model.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;

use super::ports::{
    CommitEvent, CommitReviewer, CommitVerdict, Notifier, ReflogHead, RepoInspector, RepoSource, WatchError,
    WatchedRepo,
};
use super::watch_commits::{WatchCommits, WatchConfig, WatchReport};

const POLL: Duration = Duration::from_millis(10);
/// Long enough for several poll cycles.
const SETTLE: Duration = Duration::from_millis(80);
const WAIT_LIMIT: Duration = Duration::from_secs(5);
/// Sha of the seeded `main` branch, used as the base of every reviewed feature branch.
const MAIN_SHA: &str = "main0";

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[derive(Default)]
struct InspectorState {
    refs: HashMap<PathBuf, HashMap<String, String>>,
    reflog: HashMap<(PathBuf, String), ReflogHead>,
    merge_bases: HashMap<(PathBuf, String, String), Option<String>>,
    fail_refs: HashSet<PathBuf>,
    fail_reflog: HashSet<PathBuf>,
    /// `.qgate.toml` `watch.base_branch` per `(repo, commit)` it is read at.
    repo_bases: HashMap<(PathBuf, String), String>,
}

#[derive(Clone, Default)]
struct FakeInspector(Arc<Mutex<InspectorState>>);

fn branch_of(ref_name: &str) -> String {
    ref_name.strip_prefix("refs/remotes/origin/").unwrap_or(ref_name).to_owned()
}

fn base_key(repo: &Path, a: &str, b: &str) -> (PathBuf, String, String) {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    (repo.to_path_buf(), lo.to_owned(), hi.to_owned())
}

impl FakeInspector {
    /// Records a push of `sha` to `branch` (reflog subject `update by push`, just now).
    fn push(&self, repo: &str, branch: &str, sha: &str) {
        self.push_with(repo, branch, sha, "update by push", 0);
    }

    fn push_with(&self, repo: &str, branch: &str, sha: &str, subject: &str, age_secs: i64) {
        let mut state = self.0.lock().unwrap();
        let path = PathBuf::from(repo);
        state.refs.entry(path.clone()).or_default().insert(branch.to_owned(), sha.to_owned());
        state.reflog.insert(
            (path, branch.to_owned()),
            ReflogHead { sha: sha.to_owned(), logged_at: unix_now() - age_secs, subject: subject.to_owned() },
        );
    }

    /// Sets a ref with no reflog entry (as if `core.logAllRefUpdates` were off).
    fn set_ref_no_reflog(&self, repo: &str, branch: &str, sha: &str) {
        let mut state = self.0.lock().unwrap();
        let path = PathBuf::from(repo);
        state.refs.entry(path.clone()).or_default().insert(branch.to_owned(), sha.to_owned());
        state.reflog.remove(&(path, branch.to_owned()));
    }

    /// Pushes `sha` to `branch` and records its merge base with `main` as `base`.
    fn push_feature(&self, repo: &str, branch: &str, sha: &str, base: &str) {
        self.push(repo, branch, sha);
        self.set_merge_base(repo, MAIN_SHA, sha, Some(base));
    }

    fn set_merge_base(&self, repo: &str, a: &str, b: &str, base: Option<&str>) {
        self.0.lock().unwrap().merge_bases.insert(base_key(Path::new(repo), a, b), base.map(str::to_owned));
    }

    fn remove_branch(&self, repo: &str, branch: &str) {
        let mut state = self.0.lock().unwrap();
        if let Some(branches) = state.refs.get_mut(Path::new(repo)) {
            branches.remove(branch);
        }
    }

    fn fail_refs(&self, repo: &str, failing: bool) {
        let mut state = self.0.lock().unwrap();
        toggle(&mut state.fail_refs, repo, failing);
    }

    fn fail_reflog(&self, repo: &str, failing: bool) {
        let mut state = self.0.lock().unwrap();
        toggle(&mut state.fail_reflog, repo, failing);
    }
}

fn toggle(set: &mut HashSet<PathBuf>, repo: &str, on: bool) {
    if on {
        set.insert(PathBuf::from(repo));
    } else {
        set.remove(Path::new(repo));
    }
}

impl RepoInspector for FakeInspector {
    async fn remote_refs(&self, repo: &Path, _remote: &str) -> Result<Vec<(String, String)>, WatchError> {
        let state = self.0.lock().unwrap();
        if state.fail_refs.contains(repo) {
            return Err(WatchError::Git("unavailable".into()));
        }
        let mut refs: Vec<(String, String)> = state
            .refs
            .get(repo)
            .map(|branches| branches.iter().map(|(b, s)| (b.clone(), s.clone())).collect())
            .unwrap_or_default();
        refs.sort();
        Ok(refs)
    }

    async fn ref_reflog(&self, repo: &Path, ref_name: &str) -> Result<Option<ReflogHead>, WatchError> {
        let state = self.0.lock().unwrap();
        if state.fail_reflog.contains(repo) {
            return Err(WatchError::Git("index.lock exists".into()));
        }
        Ok(state.reflog.get(&(repo.to_path_buf(), branch_of(ref_name))).cloned())
    }

    async fn repo_base_branch(&self, repo: &Path, rev: &str) -> Option<String> {
        self.0.lock().unwrap().repo_bases.get(&(repo.to_path_buf(), rev.to_owned())).cloned()
    }

    async fn merge_base(&self, repo: &Path, a: &str, b: &str) -> Result<Option<String>, WatchError> {
        Ok(self.0.lock().unwrap().merge_bases.get(&base_key(repo, a, b)).cloned().flatten())
    }
}

#[derive(Clone)]
struct FakeReviewer {
    calls: Arc<Mutex<Vec<CommitEvent>>>,
    outcomes: Arc<Mutex<HashMap<String, Result<CommitVerdict, String>>>>,
    gate: Option<Arc<Semaphore>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

impl FakeReviewer {
    fn new() -> Self {
        Self {
            calls: Arc::default(),
            outcomes: Arc::default(),
            gate: None,
            active: Arc::default(),
            max_active: Arc::default(),
        }
    }

    fn gated() -> Self {
        Self { gate: Some(Arc::new(Semaphore::new(0))), ..Self::new() }
    }

    fn release(&self, reviews: usize) {
        self.gate.as_ref().expect("gated reviewer").add_permits(reviews);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|e| e.sha.clone()).collect()
    }

    fn events(&self) -> Vec<CommitEvent> {
        self.calls.lock().unwrap().clone()
    }

    /// `(branch, base, sha)` of every reviewed push.
    fn reviews(&self) -> Vec<(Option<String>, Option<String>, String)> {
        self.calls.lock().unwrap().iter().map(|e| (e.branch.clone(), e.base.clone(), e.sha.clone())).collect()
    }

    fn set_outcome(&self, sha: &str, outcome: Result<CommitVerdict, String>) {
        self.outcomes.lock().unwrap().insert(sha.to_owned(), outcome);
    }
}

impl CommitReviewer for FakeReviewer {
    async fn review_commit(&self, event: &CommitEvent) -> Result<CommitVerdict, WatchError> {
        self.calls.lock().unwrap().push(event.clone());
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.acquire().await.expect("gate open").forget();
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        assert_ne!(event.sha, "boom", "reviewer crashed");
        let outcome = self.outcomes.lock().unwrap().get(&event.sha).cloned();
        match outcome {
            Some(Ok(verdict)) => Ok(verdict),
            Some(Err(message)) => Err(WatchError::Review(message)),
            None => Ok(CommitVerdict { passed: true, summary: "ok".into(), ..Default::default() }),
        }
    }
}

#[derive(Clone, Default)]
struct FakeNotifier(Arc<Mutex<Vec<(String, String)>>>);

impl FakeNotifier {
    fn titles(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().map(|(t, _)| t.clone()).collect()
    }
}

impl Notifier for FakeNotifier {
    async fn notify(&self, title: &str, body: &str) {
        self.0.lock().unwrap().push((title.to_owned(), body.to_owned()));
    }
}

#[derive(Clone, Default)]
struct FakeSource(Arc<Mutex<Vec<WatchedRepo>>>);

impl FakeSource {
    fn with(repos: &[&str]) -> Self {
        let source = Self::default();
        for repo in repos {
            source.add(repo);
        }
        source
    }

    fn add(&self, repo: &str) {
        self.0.lock().unwrap().push(WatchedRepo::from_path(PathBuf::from(repo)));
    }
}

impl RepoSource for FakeSource {
    async fn list(&self) -> Result<Vec<WatchedRepo>, WatchError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

struct Running {
    stop: oneshot::Sender<()>,
    handle: JoinHandle<WatchReport>,
}

impl Running {
    async fn stop(self) -> WatchReport {
        let _ = self.stop.send(());
        tokio::time::timeout(WAIT_LIMIT, self.handle).await.expect("run stops").expect("no panic")
    }
}

fn config() -> WatchConfig {
    WatchConfig { poll_interval: POLL, ..WatchConfig::default() }
}

fn start(
    inspector: &FakeInspector,
    reviewer: &FakeReviewer,
    notifier: &FakeNotifier,
    source: &FakeSource,
    config: WatchConfig,
) -> Running {
    let use_case = WatchCommits::new(inspector.clone(), reviewer.clone(), notifier.clone(), source.clone(), config);
    let (stop, stopped) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        use_case
            .run(async {
                let _ = stopped.await;
            })
            .await
    });
    Running { stop, handle }
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT_LIMIT;
    while !condition() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn settle() {
    tokio::time::sleep(SETTLE).await;
}

struct Fixture {
    inspector: FakeInspector,
    reviewer: FakeReviewer,
    notifier: FakeNotifier,
    source: FakeSource,
}

impl Fixture {
    /// Each repo starts with a `main` branch already pushed, so it is a known repo whose base
    /// branch resolves to `main`.
    fn new(reviewer: FakeReviewer, repos: &[&str]) -> Self {
        let inspector = FakeInspector::default();
        for repo in repos {
            inspector.push(repo, "main", MAIN_SHA);
        }
        Self { inspector, reviewer, notifier: FakeNotifier::default(), source: FakeSource::with(repos) }
    }

    fn start(&self, config: WatchConfig) -> Running {
        start(&self.inspector, &self.reviewer, &self.notifier, &self.source, config)
    }

    /// Starts the watcher and waits for the initial baseline poll to settle.
    async fn started(&self) -> Running {
        let running = self.start(config());
        settle().await;
        running
    }
}

fn review(branch: &str, base: Option<&str>, sha: &str) -> (Option<String>, Option<String>, String) {
    (Some(branch.to_owned()), base.map(str::to_owned), sha.to_owned())
}

#[tokio::test]
async fn a_pushed_branch_is_reviewed_once_against_its_merge_base() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    settle().await;

    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("base1"), "c1")]);
    assert_eq!(running.stop().await, WatchReport { reviews_completed: 1, reviews_aborted: 0 });
}

#[tokio::test]
async fn branches_present_at_startup_are_never_reviewed() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    fx.inspector.push("/a", "feature", "old");
    let running = fx.started().await;

    assert!(fx.reviewer.calls().is_empty(), "the baseline is only recorded");
    running.stop().await;
}

#[tokio::test]
async fn a_fetch_does_not_trigger_a_review() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    // A remote-tracking ref that moved because of a fetch, not a push.
    fx.inspector.push_with("/a", "feature", "c1", "fetch", 0);
    fx.inspector.set_merge_base("/a", MAIN_SHA, "c1", Some("base1"));
    settle().await;
    assert!(fx.reviewer.calls().is_empty(), "a fetch is not a push");
    running.stop().await;
}

#[tokio::test]
async fn a_force_push_is_reviewed() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_with("/a", "feature", "c1", "forced-update", 0);
    fx.inspector.set_merge_base("/a", MAIN_SHA, "c1", Some("base1"));
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("base1"), "c1")]);
    running.stop().await;
}

#[tokio::test]
async fn pushing_several_branches_at_once_reviews_one_per_branch() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    // `git push --all`: two refs move in one poll.
    fx.inspector.push_feature("/a", "feat-1", "c1", "base1");
    fx.inspector.push_feature("/a", "feat-2", "c2", "base2");
    wait_until("both reviews", || fx.notifier.titles().len() == 2).await;
    settle().await;

    let mut reviews = fx.reviewer.reviews();
    reviews.sort();
    assert_eq!(reviews, vec![review("feat-1", Some("base1"), "c1"), review("feat-2", Some("base2"), "c2")]);
    running.stop().await;
}

#[tokio::test]
async fn deleting_a_branch_is_ignored() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    fx.inspector.remove_branch("/a", "feature");
    settle().await;

    assert_eq!(fx.reviewer.calls(), vec!["c1"], "the delete produced no error and no review");
    running.stop().await;
}

#[tokio::test]
async fn re_pushing_the_same_sha_is_deduplicated() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    wait_until("first review", || fx.notifier.titles().len() == 1).await;
    // Delete and re-push the very same sha: the ref reappears but the push is already known.
    fx.inspector.remove_branch("/a", "feature");
    settle().await;
    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    settle().await;

    assert_eq!(fx.reviewer.calls(), vec!["c1"]);
    running.stop().await;
}

#[tokio::test]
async fn a_new_push_of_the_same_branch_is_reviewed_again() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    wait_until("first", || fx.notifier.titles().len() == 1).await;
    fx.inspector.push_feature("/a", "feature", "c2", "base1");
    wait_until("second", || fx.notifier.titles().len() == 2).await;

    assert_eq!(
        fx.reviewer.reviews(),
        vec![review("feature", Some("base1"), "c1"), review("feature", Some("base1"), "c2"),]
    );
    running.stop().await;
}

#[tokio::test]
async fn no_merge_base_reviews_the_whole_branch_with_a_note() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    // Push with no configured merge base (orphan/unrelated history).
    fx.inspector.push("/a", "feature", "c1");
    wait_until("review", || fx.notifier.titles().len() == 1).await;

    let event = fx.reviewer.events().pop().unwrap();
    assert_eq!(event.base, None);
    assert_eq!(event.note.as_deref(), Some("no merge-base; reviewing whole branch"));
    running.stop().await;
}

#[tokio::test]
async fn an_empty_branch_after_rebase_is_a_no_op() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    // Branch tip equals its merge base with main: nothing new to review.
    fx.inspector.push_feature("/a", "feature", "c1", "c1");
    settle().await;
    assert!(fx.reviewer.calls().is_empty());

    // A later real push of the branch is still reviewed.
    fx.inspector.push_feature("/a", "feature", "c2", "base1");
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("base1"), "c2")]);
    running.stop().await;
}

#[tokio::test]
async fn a_ref_without_reflog_is_reviewed_with_a_note() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.set_ref_no_reflog("/a", "feature", "c1");
    fx.inspector.set_merge_base("/a", MAIN_SHA, "c1", Some("base1"));
    wait_until("review", || fx.notifier.titles().len() == 1).await;

    let event = fx.reviewer.events().pop().unwrap();
    assert_eq!(event.base.as_deref(), Some("base1"));
    assert_eq!(event.note.as_deref(), Some("push not confirmed (reflog unavailable)"));
    running.stop().await;
}

#[tokio::test]
async fn a_push_to_the_base_branch_is_skipped() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push("/a", "main", "main1");
    settle().await;
    assert!(fx.reviewer.calls().is_empty(), "a push to main is never reviewed");
    running.stop().await;
}

#[tokio::test]
async fn a_configured_base_branch_is_skipped_and_used_as_base() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    fx.inspector.push("/a", "develop", "dev0");
    let config = WatchConfig { base_branch: "develop".into(), ..config() };
    let running = fx.start(config);
    settle().await;

    fx.inspector.push("/a", "develop", "dev1");
    settle().await;
    assert!(fx.reviewer.calls().is_empty(), "configured base branch is not reviewed");

    fx.inspector.push("/a", "feature", "c1");
    fx.inspector.set_merge_base("/a", "dev1", "c1", Some("devbase"));
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("devbase"), "c1")]);
    running.stop().await;
}

#[tokio::test]
async fn a_repo_base_branch_wins_over_the_server_setting_and_the_default() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    fx.inspector.push("/a", "develop", "dev1");
    // Declared in the repo's config at the tip of the server-configured base ("release"), not at the push.
    fx.inspector.push("/a", "release", "rel1");
    fx.inspector.0.lock().unwrap().repo_bases.insert((PathBuf::from("/a"), "rel1".into()), "develop".into());
    let running = fx.start(WatchConfig { base_branch: "release".into(), ..config() });
    settle().await;

    fx.inspector.push("/a", "feature", "c1");
    fx.inspector.set_merge_base("/a", "dev1", "c1", Some("devbase"));
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("devbase"), "c1")], "repo override wins");

    fx.inspector.push("/a", "develop", "dev2");
    settle().await;
    assert_eq!(fx.reviewer.calls().len(), 1, "a push to the overriding base branch is not reviewed");
    running.stop().await;
}

#[tokio::test]
async fn without_a_repo_base_branch_the_server_setting_then_the_default_apply() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;
    fx.inspector.push("/a", "feature", "c1");
    fx.inspector.set_merge_base("/a", MAIN_SHA, "c1", Some("mainbase"));
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("mainbase"), "c1")], "default main/master");
    running.stop().await;
}

#[tokio::test]
async fn a_pushed_commit_cannot_choose_its_own_base_branch() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;
    // The override is only visible at the pushed commit "c1": it must not be consulted.
    fx.inspector.0.lock().unwrap().repo_bases.insert((PathBuf::from("/a"), "c1".into()), "feature".into());
    fx.inspector.push("/a", "feature", "c1");
    fx.inspector.set_merge_base("/a", MAIN_SHA, "c1", Some("mainbase"));
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("mainbase"), "c1")]);
    running.stop().await;
}

#[tokio::test]
async fn an_old_push_is_skipped() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_with("/a", "feature", "stale", "update by push", 2 * 60 * 60);
    fx.inspector.set_merge_base("/a", MAIN_SHA, "stale", Some("base1"));
    settle().await;
    assert!(fx.reviewer.calls().is_empty(), "push older than max_commit_age");

    // The skip is definitive: a newer push is still reviewed.
    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.calls(), vec!["c1"]);
    running.stop().await;
}

#[tokio::test]
async fn pushes_during_a_review_are_coalesced_to_the_newest() {
    let fx = Fixture::new(FakeReviewer::gated(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    wait_until("c1 review started", || fx.reviewer.calls().len() == 1).await;
    for sha in ["c2", "c3", "c4"] {
        fx.inspector.push_feature("/a", "feature", sha, "base1");
        settle().await;
    }
    fx.reviewer.release(10);
    wait_until("queued review", || fx.notifier.titles().len() == 2).await;
    settle().await;

    assert_eq!(
        fx.reviewer.reviews(),
        vec![review("feature", Some("base1"), "c1"), review("feature", Some("base1"), "c4"),]
    );
    running.stop().await;
}

#[tokio::test]
async fn concurrent_reviews_are_limited() {
    let fx = Fixture::new(FakeReviewer::gated(), &["/a", "/b", "/c"]);
    let running = fx.started().await;

    for repo in ["/a", "/b", "/c"] {
        fx.inspector.push_feature(repo, "feature", &format!("{repo}-1"), "base");
    }
    wait_until("two reviews running", || fx.reviewer.calls().len() == 2).await;
    settle().await;
    assert_eq!(fx.reviewer.calls().len(), 2, "third review waits for a permit");

    fx.reviewer.release(3);
    wait_until("all reviews", || fx.notifier.titles().len() == 3).await;
    assert_eq!(fx.reviewer.max_active.load(Ordering::SeqCst), 2);
    running.stop().await;
}

#[tokio::test]
async fn transient_refs_failure_keeps_the_baseline() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.fail_refs("/a", true);
    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    settle().await;
    assert!(fx.reviewer.calls().is_empty(), "no review while listing refs fails");
    fx.inspector.fail_refs("/a", false);

    wait_until("review after recovery", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.calls(), vec!["c1"]);
    running.stop().await;
}

#[tokio::test]
async fn transient_reflog_failure_is_retried_on_the_next_poll() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.fail_reflog("/a", true);
    fx.inspector.push_feature("/a", "feature", "c1", "base1");
    settle().await;
    assert!(fx.reviewer.calls().is_empty(), "no review while the reflog read fails");
    fx.inspector.fail_reflog("/a", false);

    wait_until("review after recovery", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.reviews(), vec![review("feature", Some("base1"), "c1")]);
    running.stop().await;
}

#[tokio::test]
async fn notifies_pass_fail_and_error() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let failed = CommitVerdict { passed: false, blocking_issues: 2, ..Default::default() };
    fx.reviewer.set_outcome("fail0000", Ok(failed));
    fx.reviewer.set_outcome("err00000", Err("API down".into()));
    let running = fx.started().await;

    for (sha, branch, count) in [("pass0000", "f1", 1), ("fail0000", "f2", 2), ("err00000", "f3", 3)] {
        fx.inspector.push_feature("/a", branch, sha, "base1");
        wait_until(sha, || fx.notifier.titles().len() == count).await;
    }

    assert_eq!(
        fx.notifier.titles(),
        vec![
            "✅ qgate: a pass000 passed",
            "❌ qgate: a fail000 — 2 blocking issues — open dashboard",
            "⚠️ qgate: a err0000 review error",
        ]
    );
    running.stop().await;
}

#[tokio::test]
async fn a_newly_added_repository_is_baselined_before_it_is_reviewed() {
    let fx = Fixture::new(FakeReviewer::new(), &[]);
    let running = fx.started().await;

    fx.inspector.push("/late", "feature", "already-there");
    fx.source.add("/late");
    settle().await;
    assert!(fx.reviewer.calls().is_empty(), "a newly added repo starts from a baseline");

    fx.inspector.push_feature("/late", "feature", "c1", "base1");
    wait_until("review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.calls(), vec!["c1"]);
    running.stop().await;
}

#[tokio::test]
async fn a_crashed_review_does_not_block_the_branch() {
    let fx = Fixture::new(FakeReviewer::new(), &["/a"]);
    let running = fx.started().await;

    fx.inspector.push_feature("/a", "feature", "boom", "base1");
    wait_until("crashed review", || fx.reviewer.calls().len() == 1).await;
    settle().await;
    fx.inspector.push_feature("/a", "feature", "c2", "base1");

    wait_until("next review", || fx.notifier.titles().len() == 1).await;
    assert_eq!(fx.reviewer.calls(), vec!["boom", "c2"]);
    running.stop().await;
}

#[tokio::test]
async fn shutdown_waits_for_running_review_and_drops_queued_ones() {
    let fx = Fixture::new(FakeReviewer::gated(), &["/a", "/b"]);
    let config = WatchConfig { max_concurrent_reviews: 1, ..config() };
    let running = fx.start(config);
    settle().await;

    fx.inspector.push_feature("/a", "feature", "a1", "base1");
    wait_until("review started", || fx.reviewer.calls().len() == 1).await;
    fx.inspector.push_feature("/a", "feature", "a2", "base1");
    fx.inspector.push_feature("/b", "feature", "b1", "base1");
    settle().await;

    let _ = running.stop.send(());
    settle().await;
    assert!(!running.handle.is_finished(), "shutdown waits for the running review");
    fx.reviewer.release(10);
    let report = tokio::time::timeout(WAIT_LIMIT, running.handle).await.unwrap().unwrap();

    assert_eq!(report, WatchReport { reviews_completed: 1, reviews_aborted: 0 });
    assert_eq!(fx.reviewer.calls(), vec!["a1"], "queued and waiting reviews are not started");
}

#[tokio::test]
async fn shutdown_aborts_reviews_after_grace_period() {
    let fx = Fixture::new(FakeReviewer::gated(), &["/a"]);
    let config = WatchConfig { shutdown_grace: Duration::from_millis(50), ..config() };
    let running = fx.start(config);
    settle().await;

    fx.inspector.push_feature("/a", "feature", "a1", "base1");
    wait_until("review started", || fx.reviewer.calls().len() == 1).await;

    let report = running.stop().await;
    assert_eq!(report, WatchReport { reviews_completed: 0, reviews_aborted: 1 });
    assert!(fx.notifier.titles().is_empty());
}

#[test]
fn default_max_commit_age_is_one_hour() {
    assert_eq!(WatchConfig::default().max_commit_age, Duration::from_secs(60 * 60));
}
