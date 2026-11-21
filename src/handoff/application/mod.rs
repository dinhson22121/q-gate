//! `ExportReview`: writes the review hand-off files (Markdown, optional JSON, `latest`) after a scan, merges the
//! review's prevention rules into the per-repo rules file and prunes old generated files. Other contexts use the hand-off context only through here.

mod export;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use crate::handoff::domain::{
    BlockingRules, HandoffDocument, HandoffInput, JSON_SCHEMA, RunMeta, render_json, render_markdown,
};
pub use export::{ExportOutcome, ExportReview, SkipReason};

const NATIVE_DIR: [&str; 2] = [".qgate", "reviews"];

/// A file directly inside a listed folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub name: String,
    pub modified: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SinkError(pub String);

/// Where review files go. Paths are relative, `/`-separated, and must stay inside the sink's root.
pub trait ReviewFileSink: Send + Sync {
    /// Replaces `rel_path` atomically, creating parent folders.
    fn write(&self, rel_path: &str, bytes: &[u8]) -> Result<(), SinkError>;
    /// Contents of a regular file, `None` when it does not exist; files over `max_bytes` are an error.
    fn read(&self, rel_path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, SinkError>;
    /// Regular files directly inside `dir_prefix` (empty when the folder does not exist).
    fn list(&self, dir_prefix: &str) -> Result<Vec<StoredFile>, SinkError>;
    /// Deletes a regular file; a missing file is not an error.
    fn delete(&self, rel_path: &str) -> Result<(), SinkError>;
}

/// Review-file folder for native `qgate review` (outside Docker): `<repo_root>/.qgate/reviews`.
/// Add `.qgate/reviews/` to the repository's `.gitignore`.
pub fn native_default_dir(repo_root: &Path) -> PathBuf {
    NATIVE_DIR.iter().fold(repo_root.to_path_buf(), |path, part| path.join(part))
}
