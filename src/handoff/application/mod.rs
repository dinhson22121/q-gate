mod export;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use crate::handoff::domain::{
    BlockingRules, HandoffDocument, HandoffInput, JSON_SCHEMA, RunMeta, render_json, render_markdown,
};
pub use export::{ExportOutcome, ExportReview, SkipReason};

use crate::review::application::ReviewReport;

pub fn render_review_markdown(meta: RunMeta, report: &ReviewReport) -> String {
    render_markdown(&HandoffDocument::build(&HandoffInput { meta, report, rules: BlockingRules::default() }))
}

const NATIVE_DIR: [&str; 2] = [".qgate", "reviews"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub name: String,
    pub modified: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SinkError(pub String);

pub trait ReviewFileSink: Send + Sync {
    fn write(&self, rel_path: &str, bytes: &[u8]) -> Result<(), SinkError>;
    fn read(&self, rel_path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, SinkError>;
    fn list(&self, dir_prefix: &str) -> Result<Vec<StoredFile>, SinkError>;
    fn delete(&self, rel_path: &str) -> Result<(), SinkError>;
}

pub fn native_default_dir(repo_root: &Path) -> PathBuf {
    NATIVE_DIR.iter().fold(repo_root.to_path_buf(), |path, part| path.join(part))
}
