//! `WatchCommits`: polls watched repositories for pushed branches and reviews each whole branch in
//! the background, without ever blocking polling on a review. The trigger is a local `git push`
//! (a remote-tracking ref moving by `update by push`/`forced-update`), never a plain commit.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, interval, timeout};

use super::ports::{CommitEvent, CommitReviewer, Notifier, RepoInspector, RepoSource, WatchedRepo};
use crate::watch::domain::detection::{RefId, detect_ref_moves, resolve_base_branch};
use crate::watch::domain::freshness::{DEFAULT_MAX_COMMIT_AGE, RefMove, classify_ref_move};
use crate::watch::domain::message::review_notification;
use crate::watch::domain::recent::{DEFAULT_RECENT_CAPACITY, RecentCommits};
use crate::watch::domain::repo::short_sha;

/// Note attached when a ref has no reflog and the push cannot be confirmed.
const NO_REFLOG_NOTE: &str = "push not confirmed (reflog unavailable)";
/// Note attached when there is no merge base with the base branch.
const NO_MERGE_BASE_NOTE: &str = "no merge-base; reviewing whole branch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchConfig {
    /// Delay between two polls.
    pub poll_interval: Duration,
    /// Maximum number of reviews running at the same time (clamped to at least 1).
    pub max_concurrent_reviews: usize,
    /// How long shutdown waits for running reviews before aborting them.
    pub shutdown_grace: Duration,
    /// How many handled pushes are remembered for deduplication.
    pub dedup_capacity: usize,
    /// Pushes older than this (reflog entry time) are not reviewed.
    pub max_commit_age: Duration,
    /// Git remote whose tracking refs are watched.
    pub remote: String,
    /// Base branch for merge-base; empty = auto-resolve `main` then `master`. A push to it is skipped.
    pub base_branch: String,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(2),
            max_concurrent_reviews: 2,
            shutdown_grace: Duration::from_secs(30),
            dedup_capacity: DEFAULT_RECENT_CAPACITY,
            max_commit_age: DEFAULT_MAX_COMMIT_AGE,
            remote: "origin".into(),
            base_branch: String::new(),
        }
    }
}

/// What happened during a `run`, returned after shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WatchReport {
    /// Reviews that finished (passed, failed or errored) and were notified.
    pub reviews_completed: usize,
    /// Reviews still running when the shutdown grace period expired.
    pub reviews_aborted: usize,
}

pub struct WatchCommits<I, C, N, S> {
    inspector: Arc<I>,
    reviewer: Arc<C>,
    notifier: Arc<N>,
    source: S,
    config: WatchConfig,
}

/// Per-branch review slots: a key exists while a review task runs for that `(repo, branch)`; the
/// value is the push waiting for it (a later push of the same branch is coalesced into it).
type Slots = Mutex<HashMap<RefId, Option<CommitEvent>>>;

struct Shared<C, N> {
    reviewer: Arc<C>,
    notifier: Arc<N>,
    slots: Slots,
    permits: Semaphore,
    stopping: AtomicBool,
    completed: AtomicUsize,
}

/// Mutable state of the polling loop.
struct PollState {
    repos: Vec<WatchedRepo>,
    /// Last observed sha per watched ref.
    baseline: HashMap<RefId, String>,
    /// Repositories seen in an earlier poll: a new ref of one of them is a push; a new repo is only
    /// baselined, so its existing branches are never reviewed as if just pushed.
    observed_repos: HashSet<PathBuf>,
    recent: RecentCommits,
    /// Last error logged per subject, so a persistent failure is logged once.
    last_errors: HashMap<String, String>,
}

/// A repository's remote-tracking refs for one poll, used to resolve its base branch.
type RepoRefs = HashMap<PathBuf, Vec<(String, String)>>;

impl<I, C, N, S> WatchCommits<I, C, N, S>
where
    I: RepoInspector,
    C: CommitReviewer,
    N: Notifier,
    S: RepoSource,
{
    pub fn new(inspector: I, reviewer: C, notifier: N, source: S, config: WatchConfig) -> Self {
        Self {
            inspector: Arc::new(inspector),
            reviewer: Arc::new(reviewer),
            notifier: Arc::new(notifier),
            source,
            config,
        }
    }

    /// Watches until `shutdown` completes, then waits up to `shutdown_grace` for running
    /// reviews (queued ones are dropped) and aborts the rest.
    pub async fn run(&self, shutdown: impl Future<Output = ()>) -> WatchReport {
        let shared = Arc::new(Shared {
            reviewer: Arc::clone(&self.reviewer),
            notifier: Arc::clone(&self.notifier),
            slots: Mutex::new(HashMap::new()),
            permits: Semaphore::new(self.config.max_concurrent_reviews.max(1)),
            stopping: AtomicBool::new(false),
            completed: AtomicUsize::new(0),
        });
        let mut tasks = JoinSet::new();
        let mut state = PollState {
            repos: Vec::new(),
            baseline: HashMap::new(),
            observed_repos: HashSet::new(),
            recent: RecentCommits::new(self.config.dedup_capacity),
            last_errors: HashMap::new(),
        };
        let mut ticker = interval(self.config.poll_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = ticker.tick() => {}
            }
            let events = tokio::select! {
                biased;
                () = &mut shutdown => break,
                events = self.poll_once(&mut state) => events,
            };
            for event in events {
                schedule(&shared, &mut tasks, event);
            }
            while tasks.try_join_next().is_some() {}
        }

        drain(&shared, tasks, self.config.shutdown_grace).await
    }

    /// Polls every repository once and returns the pushed branches to review.
    async fn poll_once(&self, state: &mut PollState) -> Vec<CommitEvent> {
        match self.source.list().await {
            Ok(repos) => {
                clear_error(state, "registry");
                state.repos = repos;
            }
            Err(error) => log_error(state, "registry", &error.to_string()),
        }

        let mut current: HashMap<RefId, String> = HashMap::new();
        let mut carried: HashMap<RefId, String> = HashMap::new();
        let mut repo_refs: RepoRefs = HashMap::new();
        let mut by_path = HashMap::with_capacity(state.repos.len());
        let mut listed: Vec<PathBuf> = Vec::new();
        for repo in state.repos.clone() {
            let subject = repo.path.display().to_string();
            match self.inspector.remote_refs(&repo.path, &self.config.remote).await {
                Ok(refs) => {
                    clear_error(state, &subject);
                    for (branch, sha) in &refs {
                        current.insert((repo.path.clone(), branch.clone()), sha.clone());
                    }
                    repo_refs.insert(repo.path.clone(), refs);
                    listed.push(repo.path.clone());
                }
                Err(error) => {
                    log_error(state, &subject, &error.to_string());
                    // Keep the baseline so a transient failure does not hide (or replay) a push.
                    for (id, sha) in &state.baseline {
                        if id.0 == repo.path {
                            carried.insert(id.clone(), sha.clone());
                        }
                    }
                }
            }
            by_path.insert(repo.path.clone(), repo);
        }

        let previous = std::mem::take(&mut state.baseline);
        let known_before = state.observed_repos.clone();
        let mut baseline = current.clone();
        baseline.extend(carried);

        let mut events = Vec::new();
        for (id, sha, old) in detect_ref_moves(&previous, &current) {
            if !known_before.contains(&id.0) {
                continue; // newly observed repository: baseline only, never reviewed as a push
            }
            let Some(repo) = by_path.get(&id.0).cloned() else { continue };
            let Some(refs) = repo_refs.get(&id.0) else { continue };
            match self.examine_push(state, repo, &id.1, &sha, refs).await {
                Move::Review(event) => events.push(event),
                Move::Skip => {}
                Move::Retry => match old {
                    Some(previous_sha) => {
                        baseline.insert(id, previous_sha);
                    }
                    None => {
                        baseline.remove(&id);
                    }
                },
            }
        }
        state.baseline = baseline;
        state.observed_repos.extend(listed);
        events
    }

    /// Decides what to do with a moved ref. A `(repo, branch, sha)` is remembered as handled only
    /// once the answer is definitive; inspection errors ask for a retry.
    async fn examine_push(
        &self,
        state: &mut PollState,
        repo: WatchedRepo,
        branch: &str,
        sha: &str,
        refs: &[(String, String)],
    ) -> Move {
        if state.recent.contains(&repo.path, branch, sha) {
            return Move::Skip;
        }
        let label = format!("{} {branch}", repo.path.display());
        let branch_names: Vec<String> = refs.iter().map(|(b, _)| b.clone()).collect();
        let base_branch = resolve_base_branch(&branch_names, &self.config.base_branch);
        if base_branch.as_deref() == Some(branch) {
            state.recent.insert(repo.path, branch.to_owned(), sha.to_owned());
            return Move::Skip; // never review a push to the base branch
        }

        let full_ref = format!("refs/remotes/{}/{branch}", self.config.remote);
        let reflog_subject = format!("{label} (reflog)");
        let entry = match self.inspector.ref_reflog(&repo.path, &full_ref).await {
            Ok(entry) => {
                clear_error(state, &reflog_subject);
                entry
            }
            Err(error) => {
                log_error(state, &reflog_subject, &error.to_string());
                return Move::Retry;
            }
        };
        let mut notes = Vec::new();
        match classify_ref_move(entry.as_ref(), unix_now(), self.config.max_commit_age) {
            RefMove::NoReflog => notes.push(NO_REFLOG_NOTE.to_owned()),
            mv if mv.is_push() => {}
            mv => {
                eprintln!("qgate watch: {label}: not reviewing {}: {mv}", short_sha(sha));
                state.recent.insert(repo.path, branch.to_owned(), sha.to_owned());
                return Move::Skip;
            }
        }

        let base_sha = base_branch.as_ref().and_then(|bb| refs.iter().find(|(b, _)| b == bb).map(|(_, s)| s.clone()));
        let base = match &base_sha {
            Some(base_sha) => {
                let base_subject = format!("{label} (merge-base)");
                match self.inspector.merge_base(&repo.path, base_sha, sha).await {
                    Ok(base) => {
                        clear_error(state, &base_subject);
                        base
                    }
                    Err(error) => {
                        log_error(state, &base_subject, &error.to_string());
                        return Move::Retry;
                    }
                }
            }
            None => None,
        };
        if base.is_none() {
            notes.push(NO_MERGE_BASE_NOTE.to_owned());
        }

        state.recent.insert(repo.path.clone(), branch.to_owned(), sha.to_owned());
        if base.as_deref() == Some(sha) {
            // Branch tip is an ancestor of the base branch: nothing new to review.
            return Move::Skip;
        }
        Move::Review(CommitEvent {
            repo,
            sha: sha.to_owned(),
            base,
            branch: Some(branch.to_owned()),
            note: (!notes.is_empty()).then(|| notes.join("; ")),
        })
    }
}

/// Outcome of examining one moved ref.
enum Move {
    Review(CommitEvent),
    /// Definitively not worth a review (or already handled).
    Skip,
    /// Inspection failed; examine the move again on the next poll.
    Retry,
}

fn unix_now() -> i64 {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
}

/// Slot key for a push: its repository and branch.
fn slot_key(event: &CommitEvent) -> RefId {
    (event.repo.path.clone(), event.branch.clone().unwrap_or_default())
}

fn lock(slots: &Slots) -> MutexGuard<'_, HashMap<RefId, Option<CommitEvent>>> {
    slots.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts a review task for the branch, or queues the push behind the review already running for
/// that `(repo, branch)`, coalescing with any push already waiting (the newer push wins).
fn schedule<C: CommitReviewer, N: Notifier>(shared: &Arc<Shared<C, N>>, tasks: &mut JoinSet<()>, event: CommitEvent) {
    let key = slot_key(&event);
    {
        let mut slots = lock(&shared.slots);
        if let Some(waiting) = slots.get_mut(&key) {
            let merged = match waiting.take() {
                Some(older) => older.coalesce(event),
                None => event,
            };
            *waiting = Some(merged);
            return;
        }
        slots.insert(key.clone(), None);
    }
    tasks.spawn(review_repo(Arc::clone(shared), key, event));
}

/// Reviews `first`, then any push queued for the same branch meanwhile.
async fn review_repo<C: CommitReviewer, N: Notifier>(shared: Arc<Shared<C, N>>, key: RefId, first: CommitEvent) {
    let mut slot = SlotGuard { slots: &shared.slots, key: &key, armed: true };
    let mut event = first;
    loop {
        // Fails only when shutdown closed the semaphore: the review never started.
        let Ok(permit) = shared.permits.acquire().await else { return };
        let outcome = shared.reviewer.review_commit(&event).await.map_err(|e| e.to_string());
        drop(permit);
        let text = review_notification(&event, &outcome);
        shared.notifier.notify(&text.title, &text.body).await;
        shared.completed.fetch_add(1, Ordering::SeqCst);

        match take_queued(&shared, &key) {
            Some(next) => event = next,
            None => {
                slot.armed = false;
                return;
            }
        }
    }
}

/// Takes the push queued for `key`, or releases the slot when there is none (or when shutting
/// down). Both happen under one lock so a concurrent `schedule` never loses a push.
fn take_queued<C, N>(shared: &Shared<C, N>, key: &RefId) -> Option<CommitEvent> {
    let mut slots = lock(&shared.slots);
    let queued = if shared.stopping.load(Ordering::SeqCst) { None } else { slots.get_mut(key).and_then(Option::take) };
    if queued.is_none() {
        slots.remove(key);
    }
    queued
}

/// Frees the branch slot if the review task ends abnormally (panic or abort), so later pushes of
/// that branch are still reviewed.
struct SlotGuard<'a> {
    slots: &'a Slots,
    key: &'a RefId,
    armed: bool,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            lock(self.slots).remove(self.key);
        }
    }
}

async fn drain<C: CommitReviewer, N: Notifier>(
    shared: &Shared<C, N>,
    mut tasks: JoinSet<()>,
    grace: Duration,
) -> WatchReport {
    shared.stopping.store(true, Ordering::SeqCst);
    shared.permits.close();
    let finished = timeout(grace, async { while tasks.join_next().await.is_some() {} }).await;
    let reviews_aborted = if finished.is_err() {
        let running = tasks.len();
        tasks.shutdown().await;
        running
    } else {
        0
    };
    WatchReport { reviews_completed: shared.completed.load(Ordering::SeqCst), reviews_aborted }
}

fn log_error(state: &mut PollState, subject: &str, message: &str) {
    if state.last_errors.get(subject).is_some_and(|last| last == message) {
        return;
    }
    eprintln!("qgate watch: {subject}: {message}");
    state.last_errors.insert(subject.to_owned(), message.to_owned());
}

fn clear_error(state: &mut PollState, subject: &str) {
    if state.last_errors.remove(subject).is_some() {
        eprintln!("qgate watch: {subject}: recovered");
    }
}
