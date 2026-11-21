//! [`ContentIds`] over the local git executable, so results can be keyed on git object ids.

use futures::future::BoxFuture;

use super::local_git::LocalGit;
use crate::cache::application::{CacheError, ContentIds};

impl ContentIds for LocalGit {
    fn object_id<'a>(&'a self, rev: &'a str, path: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>> {
        Box::pin(async move { self.object_id_at(rev, path).await.map_err(|e| CacheError(e.to_string())) })
    }

    fn tree_id<'a>(&'a self, rev: &'a str, dir: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>> {
        Box::pin(async move { self.tree_id_at(rev, dir).await.map_err(|e| CacheError(e.to_string())) })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::review::infrastructure::local_git::DiffTarget;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@e.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn exposes_blob_and_tree_ids_and_maps_misses_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::create_dir(dir.path().join("svc")).unwrap();
        std::fs::write(dir.path().join("svc/a.txt"), "a").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        let ids: &dyn ContentIds = &LocalGit::new(dir.path().to_path_buf(), DiffTarget::WorkingTree).unwrap();

        let blob = ids.object_id("HEAD", "svc/a.txt").await.unwrap();
        let tree = ids.tree_id("HEAD", "svc").await.unwrap();
        assert!(blob.is_some() && tree.is_some() && blob != tree);
        assert!(ids.tree_id("HEAD", "").await.unwrap().is_some());
        assert_eq!(ids.object_id("HEAD", "missing").await.unwrap(), None);
        assert!(ids.object_id("--bad", "x").await.is_err());
    }
}
