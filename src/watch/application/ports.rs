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

pub trait RepoInspector: Send + Sync + 'static {
    fn remote_refs(
        &self,
        repo: &Path,
        remote: &str,
    ) -> impl Future<Output = Result<Vec<(String, String)>, WatchError>> + Send;

    fn ref_reflog(
        &self,
        repo: &Path,
        ref_name: &str,
    ) -> impl Future<Output = Result<Option<ReflogHead>, WatchError>> + Send;

    fn merge_base(
        &self,
        repo: &Path,
        a: &str,
        b: &str,
    ) -> impl Future<Output = Result<Option<String>, WatchError>> + Send;

    fn repo_base_branch(&self, _repo: &Path, _rev: &str) -> impl Future<Output = Option<String>> + Send {
        async { None }
    }
}

pub trait CommitReviewer: Send + Sync + 'static {
    fn review_commit(&self, event: &CommitEvent) -> impl Future<Output = Result<CommitVerdict, WatchError>> + Send;
}

pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, title: &str, body: &str) -> impl Future<Output = ()> + Send;
}

pub trait RepoSource: Send + Sync + 'static {
    fn list(&self) -> impl Future<Output = Result<Vec<WatchedRepo>, WatchError>> + Send;
}

pub trait RepoRegistry: RepoSource {
    fn add(&self, path: &Path) -> impl Future<Output = Result<WatchedRepo, WatchError>> + Send;

    fn remove(&self, path: &Path) -> impl Future<Output = Result<bool, WatchError>> + Send;
}
