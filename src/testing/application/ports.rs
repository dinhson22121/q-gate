use std::any::Any;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;

use crate::llm::{ContentPart, LlmError, LoopLimits, LoopOutcome, SystemBlock, ToolHandler, ToolSpec};
use crate::testing::domain::classify::RawRun;
use crate::testing::domain::profile::ProbeFs;

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

pub trait TestRunner: Send + Sync {
    fn run(&self, cwd: &Path, command: &[String], timeout: Duration) -> impl Future<Output = RawRun> + Send;
}

pub struct ScenarioSnapshot {
    path: PathBuf,
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

pub trait ScenarioSnapshots: Send + Sync {
    fn create<'a>(&'a self, repo_root: &'a Path, commit: &'a str) -> BoxFuture<'a, Result<ScenarioSnapshot, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub file: String,
    pub line: u32,
    pub text: String,
}

pub trait Workspace: ProbeFs + Send + Sync + Sized {
    fn open(root: &Path) -> Result<Self, String>;
    fn root(&self) -> &Path;
    fn list_dir(&self, rel: &str) -> Result<Vec<String>, String>;
    fn read_text(&self, rel: &str) -> Result<String, String>;
    fn search(&self, literal: &str, glob: Option<&str>, max_hits: usize) -> Result<Vec<SearchHit>, String>;
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
