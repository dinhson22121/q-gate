use std::collections::HashSet;

use crate::watch::application::ports::{RepoSource, WatchError, WatchedRepo};

#[derive(Debug)]
pub struct CompositeRegistry<A, B> {
    first: A,
    second: B,
}

impl<A: RepoSource, B: RepoSource> CompositeRegistry<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A: RepoSource, B: RepoSource> RepoSource for CompositeRegistry<A, B> {
    async fn list(&self) -> Result<Vec<WatchedRepo>, WatchError> {
        let mut repos = self.first.list().await?;
        repos.extend(self.second.list().await?);
        let mut seen = HashSet::with_capacity(repos.len());
        let mut unique = Vec::with_capacity(repos.len());
        for mut repo in repos {
            if let Ok(canonical) = tokio::fs::canonicalize(&repo.path).await {
                repo.path = canonical;
            }
            if seen.insert(repo.path.clone()) {
                unique.push(repo);
            }
        }
        Ok(unique)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct Fixed(Result<Vec<WatchedRepo>, String>);

    impl RepoSource for Fixed {
        async fn list(&self) -> Result<Vec<WatchedRepo>, WatchError> {
            self.0.clone().map_err(WatchError::Registry)
        }
    }

    fn repo(path: &std::path::Path, name: &str) -> WatchedRepo {
        WatchedRepo { path: path.to_path_buf(), name: name.into() }
    }

    #[tokio::test]
    async fn unions_and_deduplicates_by_canonical_path() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let other = PathBuf::from("/does/not/exist");
        let first = Fixed(Ok(vec![repo(&canonical, "registered")]));
        let second = Fixed(Ok(vec![repo(&dir.path().join("."), "scanned"), repo(&other, "x")]));

        let repos = CompositeRegistry::new(first, second).list().await.unwrap();

        assert_eq!(repos, vec![repo(&canonical, "registered"), repo(&other, "x")]);
    }

    #[tokio::test]
    async fn propagates_errors() {
        let ok = Fixed(Ok(vec![]));
        let failing = Fixed(Err("broken".into()));
        assert!(CompositeRegistry::new(ok, failing).list().await.is_err());
    }
}
