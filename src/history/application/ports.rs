use chrono::{DateTime, Utc};

use crate::history::domain::{ReviewRun, RunStatus, StoredRun, TicketKey};

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("storage error: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFilter {
    pub repo: Option<String>,
    pub status: Option<RunStatus>,
    pub limit: u32,
    pub offset: u32,
}

pub trait RunRepository: Send + Sync {
    fn insert(&self, run: &ReviewRun) -> Result<i64, HistoryError>;
    fn get(&self, id: i64) -> Result<Option<StoredRun>, HistoryError>;
    fn list(&self, filter: &RunFilter) -> Result<Vec<StoredRun>, HistoryError>;
    fn count(&self, filter: &RunFilter) -> Result<u64, HistoryError>;
    fn since(&self, since: DateTime<Utc>) -> Result<Vec<StoredRun>, HistoryError>;
    fn for_ticket(&self, key: &TicketKey, limit: u32) -> Result<Vec<StoredRun>, HistoryError>;
    fn repos(&self) -> Result<Vec<String>, HistoryError>;
}
