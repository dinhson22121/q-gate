use std::path::Path;

use futures::future::BoxFuture;

use super::ports::SourceError;
use crate::review::domain::change_set::ChangeSet;
use crate::review::domain::finding::Finding;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ToolOutcome {
    pub findings: Vec<Finding>,
    pub notes: Vec<String>,
}

pub struct CommitChange {
    pub head_sha: String,
    pub base_sha: Option<String>,
    pub changes: ChangeSet,
}

pub struct QualityRequest<'a> {
    pub snapshot: &'a Path,
    pub base_sha: Option<&'a str>,
    pub head_sha: &'a str,
    pub changes: &'a ChangeSet,
}

pub struct RegressionRequest<'a> {
    pub base_sha: &'a str,
    pub head_sha: &'a str,
    pub base_snapshot: &'a Path,
    pub head_snapshot: &'a Path,
    pub changes: &'a ChangeSet,
}

pub trait CommitChanges: Send + Sync {
    fn load(&self) -> BoxFuture<'_, Result<CommitChange, SourceError>>;
}

pub trait ExportedSnapshot: Send {
    fn path(&self) -> &Path;
}

pub trait SnapshotExporter: Send + Sync {
    fn export<'a>(&'a self, sha: &'a str) -> BoxFuture<'a, anyhow::Result<Box<dyn ExportedSnapshot>>>;
}

pub trait QualityChecks: Send + Sync {
    fn run<'a>(&'a self, request: &'a QualityRequest<'a>) -> BoxFuture<'a, ToolOutcome>;
}

pub trait RegressionChecks: Send + Sync {
    fn run<'a>(&'a self, request: &'a RegressionRequest<'a>) -> BoxFuture<'a, ToolOutcome>;
}
