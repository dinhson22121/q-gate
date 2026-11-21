use chrono::{DateTime, Utc};

use crate::history::domain::{ReviewRun, RunStatus, StoredRun, TicketKey};

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("storage error: {0}")]
    Storage(String),
}

/// Filter + page for listing runs. `limit` is already clamped by the use case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFilter {
    /// Matches either the repo name or the repo path.
    pub repo: Option<String>,
    pub status: Option<RunStatus>,
    pub limit: u32,
    pub offset: u32,
}

/// Persistence port for review runs. Synchronous (SQLite is); async callers
/// must run it on a blocking thread.
pub trait RunRepository: Send + Sync {
    /// Stores the run (its `id` is ignored) and returns the new id.
    fn insert(&self, run: &ReviewRun) -> Result<i64, HistoryError>;
    fn get(&self, id: i64) -> Result<Option<StoredRun>, HistoryError>;
    /// Newest first.
    fn list(&self, filter: &RunFilter) -> Result<Vec<StoredRun>, HistoryError>;
    /// Total matching `filter`'s repo/status, ignoring limit/offset.
    fn count(&self, filter: &RunFilter) -> Result<u64, HistoryError>;
    /// All runs created at or after `since`, newest first.
    fn since(&self, since: DateTime<Utc>) -> Result<Vec<StoredRun>, HistoryError>;
    /// Runs whose `ticket_key` is `key` or whose stored report mentions it, newest first, at most
    /// `limit`. May over-match (it is a pre-filter); callers decide with `mentions_ticket`.
    fn for_ticket(&self, key: &TicketKey, limit: u32) -> Result<Vec<StoredRun>, HistoryError>;
    /// Distinct repo names, sorted.
    fn repos(&self) -> Result<Vec<String>, HistoryError>;
}
