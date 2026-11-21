use std::future::Future;
use std::path::{Path, PathBuf};

use super::ports::{RepoRegistry, WatchError, WatchedRepo};
use crate::watch::domain::browse::Listing;

pub const MAX_IMPORTED_REPOS: usize = 1;

#[derive(Debug, thiserror::Error)]
pub enum ManageError {
    #[error("path is outside the repositories folder")]
    OutsideBase,
    #[error("no such folder: {0}")]
    NotFound(String),
    #[error("not a folder: {0}")]
    NotADirectory(String),
    #[error("not a git repository root (no .git inside): {0}")]
    NotGitRoot(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Watch(#[from] WatchError),
}

pub trait RepoBrowser: Send + Sync + 'static {
    fn browse(&self, path: &str) -> impl Future<Output = Result<Listing, ManageError>> + Send;

    fn resolve_repo_root(&self, path: &str) -> impl Future<Output = Result<PathBuf, ManageError>> + Send;
}

pub struct ManageRepos<R, B> {
    registry: R,
    browser: B,
}

impl<R: RepoRegistry, B: RepoBrowser> ManageRepos<R, B> {
    pub fn new(registry: R, browser: B) -> Self {
        Self { registry, browser }
    }

    pub async fn browse(&self, path: &str) -> Result<Listing, ManageError> {
        self.browser.browse(path).await
    }

    pub async fn list(&self) -> Result<Vec<WatchedRepo>, ManageError> {
        Ok(self.registry.list().await?)
    }

    pub async fn import(&self, path: &str) -> Result<WatchedRepo, ManageError> {
        let root = self.browser.resolve_repo_root(path).await?;
        let added = self.registry.add(&root).await?;
        let existing = self.registry.list().await?;
        let surplus = existing.len().saturating_sub(MAX_IMPORTED_REPOS);
        for old in existing.iter().filter(|repo| repo.path != added.path).take(surplus) {
            self.registry.remove(&old.path).await?;
        }
        Ok(added)
    }

    pub async fn remove(&self, path: &Path) -> Result<bool, ManageError> {
        Ok(self.registry.remove(path).await?)
    }
}
