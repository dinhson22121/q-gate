use std::path::PathBuf;

use futures::future::BoxFuture;

use crate::review::application::ports::{ChangeSource, SourceError};
use crate::review::application::{CommitChange, CommitChanges};
use crate::review::infrastructure::local_git::{DiffTarget, LocalGit};

pub struct GitCommitChanges {
    git: LocalGit,
    target: DiffTarget,
}

impl GitCommitChanges {
    pub fn new(root: PathBuf, target: DiffTarget) -> Result<Self, SourceError> {
        Ok(Self { git: LocalGit::new(root, target.clone())?, target })
    }
}

impl GitCommitChanges {
    async fn head(&self) -> Result<String, SourceError> {
        self.git
            .head_commit()
            .await?
            .ok_or_else(|| SourceError::InvalidInput("the repository has no commits yet".into()))
    }
}

impl CommitChanges for GitCommitChanges {
    fn load(&self) -> BoxFuture<'_, Result<CommitChange, SourceError>> {
        Box::pin(async move {
            let (head_sha, base_sha) = match &self.target {
                DiffTarget::Range { base, head } => (head.clone(), Some(base.clone())),
                DiffTarget::Commit(sha) => (sha.clone(), self.git.parent_of(sha).await?),
                DiffTarget::Base(base) => {
                    let head = self.head().await?;
                    let base = self.git.merge_base(base, &head).await?;
                    (head, base)
                }
                DiffTarget::WorkingTree => {
                    let head = self.head().await?;
                    (head.clone(), Some(head))
                }
            };
            Ok(CommitChange { head_sha, base_sha, changes: self.git.load().await? })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    async fn load(dir: &Path, target: DiffTarget) -> CommitChange {
        GitCommitChanges::new(dir.to_path_buf(), target).unwrap().load().await.unwrap()
    }

    #[tokio::test]
    async fn load_change_resolves_base_head_and_changes_for_commit_and_range() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-q", "-m", "one"]);
        let first = git_in(dir.path(), &["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("b.go"), "package b\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-q", "-m", "two"]);
        let second = git_in(dir.path(), &["rev-parse", "HEAD"]);

        let change = load(dir.path(), DiffTarget::Commit(second.clone())).await;
        assert_eq!((change.head_sha.as_str(), change.base_sha.as_deref()), (second.as_str(), Some(first.as_str())));
        assert_eq!(change.changes.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["b.go"]);

        let root = load(dir.path(), DiffTarget::Commit(first.clone())).await;
        assert_eq!(root.base_sha, None, "a root commit has no base");

        let range = DiffTarget::Range { base: first.clone(), head: second.clone() };
        let change = load(dir.path(), range).await;
        assert_eq!((change.head_sha, change.base_sha), (second, Some(first)));
        assert_eq!(change.changes.files.len(), 1);
    }

    #[tokio::test]
    async fn base_and_working_tree_targets_resolve_head_and_their_base() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-q", "-m", "one"]);
        let fork = git_in(dir.path(), &["rev-parse", "HEAD"]);
        git_in(dir.path(), &["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.path().join("b.go"), "package b\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-q", "-m", "two"]);
        let head = git_in(dir.path(), &["rev-parse", "HEAD"]);

        let change = load(dir.path(), DiffTarget::Base("main".into())).await;
        assert_eq!((change.head_sha.as_str(), change.base_sha.as_deref()), (head.as_str(), Some(fork.as_str())));

        let change = load(dir.path(), DiffTarget::WorkingTree).await;
        assert_eq!((change.head_sha.as_str(), change.base_sha.as_deref()), (head.as_str(), Some(head.as_str())));
    }

    #[tokio::test]
    async fn a_repository_without_commits_has_no_head_to_check() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q"]);

        let result = GitCommitChanges::new(dir.path().to_path_buf(), DiffTarget::WorkingTree).unwrap().load().await;

        assert!(matches!(result, Err(SourceError::InvalidInput(_))));
    }
}
