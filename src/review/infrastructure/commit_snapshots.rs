use std::path::{Path, PathBuf};

use futures::future::BoxFuture;

use super::snapshot::{Snapshot, export_commit};
use crate::review::application::{ExportedSnapshot, SnapshotExporter};

impl ExportedSnapshot for Snapshot {
    fn path(&self) -> &Path {
        Snapshot::path(self)
    }
}

pub struct GitCommitSnapshots {
    repo_root: PathBuf,
    work_dir: PathBuf,
}

impl GitCommitSnapshots {
    pub fn new(repo_root: PathBuf, work_dir: PathBuf) -> Self {
        Self { repo_root, work_dir }
    }
}

impl SnapshotExporter for GitCommitSnapshots {
    fn export<'a>(&'a self, sha: &'a str) -> BoxFuture<'a, anyhow::Result<Box<dyn ExportedSnapshot>>> {
        Box::pin(async move {
            let snapshot = export_commit(&self.repo_root, sha, &self.work_dir).await?;
            Ok(Box::new(snapshot) as Box<dyn ExportedSnapshot>)
        })
    }
}
