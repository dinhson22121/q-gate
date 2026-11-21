use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::watch::application::ports::{RepoSource, WatchError, WatchedRepo};

pub const DEFAULT_MAX_DEPTH: usize = 3;
pub const DEFAULT_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const SKIPPED_DIRS: &[&str] = &["node_modules", "target", "vendor"];

#[derive(Debug)]
pub struct ScanRepoRegistry {
    roots: Vec<PathBuf>,
    max_depth: usize,
    rescan_interval: Duration,
    cache: Mutex<Option<(Instant, Vec<WatchedRepo>)>>,
}

impl ScanRepoRegistry {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots, max_depth: DEFAULT_MAX_DEPTH, rescan_interval: DEFAULT_RESCAN_INTERVAL, cache: Mutex::new(None) }
    }

    pub fn with_max_depth(self, max_depth: usize) -> Self {
        Self { max_depth, ..self }
    }

    pub fn with_rescan_interval(self, rescan_interval: Duration) -> Self {
        Self { rescan_interval, ..self }
    }

    fn cached(&self) -> Option<Vec<WatchedRepo>> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.as_ref().filter(|(at, _)| at.elapsed() < self.rescan_interval).map(|(_, repos)| repos.clone())
    }
}

impl RepoSource for ScanRepoRegistry {
    async fn list(&self) -> Result<Vec<WatchedRepo>, WatchError> {
        if let Some(repos) = self.cached() {
            return Ok(repos);
        }
        let roots = self.roots.clone();
        let max_depth = self.max_depth;
        let repos = tokio::task::spawn_blocking(move || scan(&roots, max_depth))
            .await
            .map_err(|e| WatchError::Registry(format!("repository scan failed: {e}")))?;
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), repos.clone()));
        Ok(repos)
    }
}

fn scan(roots: &[PathBuf], max_depth: usize) -> Vec<WatchedRepo> {
    let mut found = Vec::new();
    for root in roots {
        if let Ok(root) = fs::canonicalize(root) {
            walk(&root, 0, max_depth, &mut found);
        }
    }
    found.sort();
    found.dedup();
    found.into_iter().map(WatchedRepo::from_path).collect()
}

fn walk(dir: &Path, depth: usize, max_depth: usize, found: &mut Vec<PathBuf>) {
    if fs::symlink_metadata(dir.join(".git")).is_ok() {
        found.push(dir.to_path_buf());
        return;
    }
    if depth >= max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) && !is_skipped(&entry.file_name()) {
            walk(&entry.path(), depth + 1, max_depth, found);
        }
    }
}

fn is_skipped(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_repo(path: &Path) {
        fs::create_dir_all(path.join(".git")).unwrap();
    }

    fn names(repos: &[WatchedRepo]) -> Vec<String> {
        repos.iter().map(|r| r.name.clone()).collect()
    }

    #[tokio::test]
    async fn discovers_repositories_up_to_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_repo(&root.join("a"));
        make_repo(&root.join("group/b"));
        make_repo(&root.join("x/y/c"));
        make_repo(&root.join("x/y/z/too-deep"));
        fs::create_dir_all(root.join("wt")).unwrap();
        fs::write(root.join("wt/.git"), "gitdir: /elsewhere").unwrap();

        let repos = ScanRepoRegistry::new(vec![root.to_path_buf()]).list().await.unwrap();

        assert_eq!(names(&repos), vec!["a", "b", "wt", "c"]);
        let canonical = fs::canonicalize(root).unwrap();
        assert!(repos.iter().all(|r| r.path.starts_with(&canonical)));
    }

    #[tokio::test]
    async fn skips_dependency_hidden_and_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_repo(&root.join("node_modules/pkg"));
        make_repo(&root.join("target/x"));
        make_repo(&root.join("vendor/y"));
        make_repo(&root.join(".cache/z"));
        make_repo(&root.join("outer"));
        make_repo(&root.join("outer/inner"));

        let repos = ScanRepoRegistry::new(vec![root.to_path_buf()]).list().await.unwrap();

        assert_eq!(names(&repos), vec!["outer"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        make_repo(&outside.path().join("real"));
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        let repos = ScanRepoRegistry::new(vec![dir.path().to_path_buf()]).list().await.unwrap();

        assert!(repos.is_empty());
    }

    #[tokio::test]
    async fn root_that_is_a_repository_and_missing_roots() {
        let dir = tempfile::tempdir().unwrap();
        make_repo(dir.path());
        let roots = vec![dir.path().to_path_buf(), dir.path().join("missing")];

        let repos = ScanRepoRegistry::new(roots).list().await.unwrap();

        assert_eq!(repos.len(), 1);
    }

    #[tokio::test]
    async fn caches_until_rescan_interval() {
        let dir = tempfile::tempdir().unwrap();
        let cached = ScanRepoRegistry::new(vec![dir.path().to_path_buf()]);
        let uncached = ScanRepoRegistry::new(vec![dir.path().to_path_buf()])
            .with_rescan_interval(Duration::ZERO)
            .with_max_depth(1);
        assert!(cached.list().await.unwrap().is_empty());
        assert!(uncached.list().await.unwrap().is_empty());

        make_repo(&dir.path().join("new"));

        assert!(cached.list().await.unwrap().is_empty(), "served from cache");
        assert_eq!(uncached.list().await.unwrap().len(), 1);
    }
}
