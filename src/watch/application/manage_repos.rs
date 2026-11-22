use std::future::Future;
use std::path::{Path, PathBuf};

use super::ports::{RepoRegistry, WatchError, WatchedRepo};
use crate::watch::domain::browse::Listing;

pub const MAX_IMPORTED_REPOS: usize = 50;

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
    #[error("too many repositories imported (maximum {0}); remove one first")]
    TooManyRepos(usize),
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
    max_repos: usize,
}

impl<R: RepoRegistry, B: RepoBrowser> ManageRepos<R, B> {
    pub fn new(registry: R, browser: B) -> Self {
        Self { registry, browser, max_repos: MAX_IMPORTED_REPOS }
    }

    pub fn with_max_repos(mut self, max_repos: usize) -> Self {
        self.max_repos = max_repos;
        self
    }

    pub async fn browse(&self, path: &str) -> Result<Listing, ManageError> {
        self.browser.browse(path).await
    }

    pub async fn list(&self) -> Result<Vec<WatchedRepo>, ManageError> {
        Ok(self.registry.list().await?)
    }

    pub async fn import(&self, path: &str) -> Result<WatchedRepo, ManageError> {
        let root = self.browser.resolve_repo_root(path).await?;
        let existing = self.registry.list().await?;
        if existing.len() >= self.max_repos && !existing.iter().any(|repo| repo.path == root) {
            return Err(ManageError::TooManyRepos(self.max_repos));
        }
        Ok(self.registry.add(&root).await?)
    }

    pub async fn remove(&self, path: &Path) -> Result<bool, ManageError> {
        Ok(self.registry.remove(path).await?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::watch::application::ports::RepoSource;

    #[derive(Clone, Default)]
    struct MemoryRegistry(Arc<Mutex<Vec<WatchedRepo>>>);

    impl RepoSource for MemoryRegistry {
        async fn list(&self) -> Result<Vec<WatchedRepo>, WatchError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    impl RepoRegistry for MemoryRegistry {
        async fn add(&self, path: &Path) -> Result<WatchedRepo, WatchError> {
            let mut repos = self.0.lock().unwrap();
            if let Some(existing) = repos.iter().find(|repo| repo.path == path) {
                return Ok(existing.clone());
            }
            let repo = WatchedRepo::from_path(path.to_path_buf());
            repos.push(repo.clone());
            Ok(repo)
        }

        async fn remove(&self, path: &Path) -> Result<bool, WatchError> {
            let mut repos = self.0.lock().unwrap();
            let before = repos.len();
            repos.retain(|repo| repo.path != path);
            Ok(repos.len() != before)
        }
    }

    struct EchoBrowser;

    impl RepoBrowser for EchoBrowser {
        async fn browse(&self, _path: &str) -> Result<Listing, ManageError> {
            Err(ManageError::OutsideBase)
        }

        async fn resolve_repo_root(&self, path: &str) -> Result<PathBuf, ManageError> {
            Ok(PathBuf::from("/base").join(path))
        }
    }

    fn service(max: usize) -> ManageRepos<MemoryRegistry, EchoBrowser> {
        ManageRepos::new(MemoryRegistry::default(), EchoBrowser).with_max_repos(max)
    }

    #[tokio::test]
    async fn import_is_additive_and_idempotent() {
        let service = service(5);
        service.import("one").await.unwrap();
        service.import("two").await.unwrap();
        service.import("one").await.unwrap();
        let names: Vec<_> = service.list().await.unwrap().into_iter().map(|repo| repo.name).collect();
        assert_eq!(names, ["one", "two"]);
    }

    #[tokio::test]
    async fn remove_keeps_the_other_repositories() {
        let service = service(5);
        service.import("one").await.unwrap();
        service.import("two").await.unwrap();
        assert!(service.remove(Path::new("/base/one")).await.unwrap());
        let names: Vec<_> = service.list().await.unwrap().into_iter().map(|repo| repo.name).collect();
        assert_eq!(names, ["two"]);
    }

    #[tokio::test]
    async fn import_beyond_the_cap_is_rejected_but_reimport_still_works() {
        let service = service(2);
        service.import("one").await.unwrap();
        service.import("two").await.unwrap();
        assert!(matches!(service.import("three").await, Err(ManageError::TooManyRepos(2))));
        service.import("two").await.unwrap();
        assert_eq!(service.list().await.unwrap().len(), 2);
        service.remove(Path::new("/base/one")).await.unwrap();
        service.import("three").await.unwrap();
    }
}
