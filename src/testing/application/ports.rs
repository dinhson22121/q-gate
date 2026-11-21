//! Ports of the testing context: the agent tool loop, the test process runner and the snapshot.

use std::any::Any;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;

use crate::llm::{ContentPart, LlmError, LoopLimits, LoopOutcome, SystemBlock, ToolHandler, ToolSpec};
use crate::testing::domain::classify::RawRun;
use crate::testing::domain::profile::ProbeFs;

/// Drives an agentic tool loop (Claude in production, a scripted fake in tests).
pub trait ToolLoop: Send + Sync {
    fn drive<H: ToolHandler>(
        &self,
        system: &[SystemBlock],
        user: Vec<ContentPart>,
        tools: &[ToolSpec],
        handler: &H,
        limits: LoopLimits,
    ) -> impl Future<Output = Result<LoopOutcome, LlmError>> + Send;
}

/// Runs one fixed command (no shell) with a filtered environment, process-group kill and timeout.
pub trait TestRunner: Send + Sync {
    fn run(&self, cwd: &Path, command: &[String], timeout: Duration) -> impl Future<Output = RawRun> + Send;
}

/// A writable copy of the reviewed commit owned by ONE scenario, so a scenario's test (or the build
/// scripts it triggers) cannot change what another scenario or the repository checks run against.
pub struct ScenarioSnapshot {
    path: PathBuf,
    /// Cleans the copy up when dropped.
    _guard: Box<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for ScenarioSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScenarioSnapshot").field("path", &self.path).finish_non_exhaustive()
    }
}

impl ScenarioSnapshot {
    pub fn new(path: PathBuf, guard: impl Any + Send + Sync) -> Self {
        Self { path, _guard: Box::new(guard) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Creates a fresh [`ScenarioSnapshot`] of `commit` in `repo_root`.
pub trait ScenarioSnapshots: Send + Sync {
    fn create<'a>(&'a self, repo_root: &'a Path, commit: &'a str) -> BoxFuture<'a, Result<ScenarioSnapshot, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub file: String,
    pub line: u32,
    pub text: String,
}

/// The writable snapshot of the reviewed commit. Every path is snapshot-relative and is rejected
/// when it escapes the snapshot (lexically or through a symlink).
pub trait Workspace: ProbeFs + Send + Sync + Sized {
    fn open(root: &Path) -> Result<Self, String>;
    /// Canonical snapshot root.
    fn root(&self) -> &Path;
    /// Entry names of a directory; directories end with `/`.
    fn list_dir(&self, rel: &str) -> Result<Vec<String>, String>;
    /// Full UTF-8 text of a regular file (bounded size).
    fn read_text(&self, rel: &str) -> Result<String, String>;
    /// Lines containing `literal`, in files matching `glob` (all files when `None`).
    fn search(&self, literal: &str, glob: Option<&str>, max_hits: usize) -> Result<Vec<SearchHit>, String>;
    /// Writes a regular file, creating missing directories. With `create_new` the file must not exist.
    fn write_file(&self, rel: &str, content: &str, create_new: bool) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_snapshot_exposes_its_path_and_debug_hides_the_guard() {
        let snapshot = ScenarioSnapshot::new(PathBuf::from("/work/s1"), ());
        assert_eq!(snapshot.path(), Path::new("/work/s1"));
        assert!(format!("{snapshot:?}").contains("/work/s1"));
    }
}
