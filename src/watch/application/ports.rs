//! Ports of the watch context. Infrastructure adapters implement them; tests use fakes.

use std::future::Future;
use std::path::Path;

pub use crate::watch::domain::freshness::ReflogHead;
pub use crate::watch::domain::repo::{CommitEvent, CommitVerdict, WatchedRepo};

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("git failed: {0}")]
    Git(String),
    #[error("not a git repository root: {0}")]
    InvalidRepo(String),
    #[error("repository registry error: {0}")]
    Registry(String),
    #[error("review failed: {0}")]
    Review(String),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("another qgate watcher is already running{}", pid.map(|p| format!(" (pid {p})")).unwrap_or_default())]
    AlreadyRunning { pid: Option<u32> },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Reads the remote-tracking state of a repository.
pub trait RepoInspector: Send + Sync + 'static {
    /// Remote-tracking branches under `refs/remotes/<remote>/` as `(branch, sha)` pairs, excluding
    /// the symbolic `<remote>/HEAD`. Empty when the remote has no tracked branches.
    fn remote_refs(
        &self,
        repo: &Path,
        remote: &str,
    ) -> impl Future<Output = Result<Vec<(String, String)>, WatchError>> + Send;

    /// Latest reflog entry of a ref (`<ref>@{0}`); `None` when the ref has no reflog
    /// (`core.logAllRefUpdates` disabled). Tells a push from a fetch/pull.
    fn ref_reflog(
        &self,
        repo: &Path,
        ref_name: &str,
    ) -> impl Future<Output = Result<Option<ReflogHead>, WatchError>> + Send;

    /// Best common ancestor of `a` and `b`, or `None` when they share no history.
    fn merge_base(
        &self,
        repo: &Path,
        a: &str,
        b: &str,
    ) -> impl Future<Output = Result<Option<String>, WatchError>> + Send;

    /// The base branch the repository itself declares (`watch.base_branch` of its `.qgate.toml`), read at the
    /// trusted commit `rev` (the tip of its default base branch, never the pushed commit). `None` when it
    /// declares none, the file is missing or unreadable, or the value is not a valid branch name.
    fn repo_base_branch(&self, _repo: &Path, _rev: &str) -> impl Future<Output = Option<String>> + Send {
        async { None }
    }
}

/// Reviews one commit (wired to the review context's use case by the composition root).
pub trait CommitReviewer: Send + Sync + 'static {
    fn review_commit(&self, event: &CommitEvent) -> impl Future<Output = Result<CommitVerdict, WatchError>> + Send;
}

/// Delivers a message to the user. Best effort: implementations never fail.
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, title: &str, body: &str) -> impl Future<Output = ()> + Send;
}

/// Lists the repositories to watch. Called every poll cycle, so it must be cheap.
pub trait RepoSource: Send + Sync + 'static {
    fn list(&self) -> impl Future<Output = Result<Vec<WatchedRepo>, WatchError>> + Send;
}

/// A repository list the user edits explicitly (`qgate watch add|remove`).
pub trait RepoRegistry: RepoSource {
    /// Registers the repository rooted at `path`; idempotent.
    fn add(&self, path: &Path) -> impl Future<Output = Result<WatchedRepo, WatchError>> + Send;

    /// Unregisters `path`; returns false when it was not registered.
    fn remove(&self, path: &Path) -> impl Future<Output = Result<bool, WatchError>> + Send;
}
