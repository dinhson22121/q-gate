//! `RepoInspector` backed by the local `git` executable.

use std::path::Path;

use super::git::{GIT_QUIET_MISS, failure, run_git};
use crate::watch::application::ports::{RepoInspector, WatchError};
use crate::watch::domain::freshness::ReflogHead;

/// Field separator in the ref/reflog queries (ASCII unit separator).
const FIELD_SEP: char = '\u{1f}';

#[derive(Debug, Clone, Default)]
pub struct GitRepoInspector;

impl GitRepoInspector {
    pub fn new() -> Self {
        Self
    }
}

/// Parses one `%(refname)\x1f%(objectname)` line into `(branch, sha)`, stripping the remote prefix
/// and skipping the symbolic `<remote>/HEAD`.
fn parse_ref_line<'a>(line: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let (refname, sha) = line.split_once(FIELD_SEP)?;
    let branch = refname.strip_prefix(prefix)?;
    if branch.is_empty() || branch == "HEAD" || sha.is_empty() {
        return None;
    }
    Some((branch, sha))
}

/// Extracts the reflog entry's unix timestamp from a `%gd` value like `origin/feature@{1700000000}`.
fn parse_reflog_time(gd: &str) -> Option<i64> {
    let start = gd.rfind("@{")? + 2;
    let end = gd[start..].find('}')? + start;
    gd[start..end].parse().ok()
}

/// Parses a `%H\x1f%gd\x1f%gs` reflog line.
fn parse_reflog_line(stdout: &str) -> Option<ReflogHead> {
    let line = stdout.lines().next()?;
    let mut fields = line.splitn(3, FIELD_SEP);
    let sha = fields.next()?.trim().to_owned();
    let logged_at = parse_reflog_time(fields.next()?)?;
    let subject = fields.next().unwrap_or_default().to_owned();
    (!sha.is_empty()).then_some(ReflogHead { sha, logged_at, subject })
}

impl RepoInspector for GitRepoInspector {
    async fn remote_refs(&self, repo: &Path, remote: &str) -> Result<Vec<(String, String)>, WatchError> {
        let namespace = format!("refs/remotes/{remote}");
        let prefix = format!("{namespace}/");
        let args = ["for-each-ref", "--format=%(refname)\u{1f}%(objectname)", &namespace];
        let output = run_git(repo, &args).await?;
        if !output.status.success() {
            return Err(failure(&args, &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .filter_map(|line| parse_ref_line(line, &prefix))
            .map(|(branch, sha)| (branch.to_owned(), sha.to_owned()))
            .collect())
    }

    async fn ref_reflog(&self, repo: &Path, ref_name: &str) -> Result<Option<ReflogHead>, WatchError> {
        let args = ["log", "-g", "-1", "--date=unix", "--format=%H\u{1f}%gd\u{1f}%gs", ref_name];
        let output = run_git(repo, &args).await?;
        if !output.status.success() {
            // No reflog for the ref (core.logAllRefUpdates disabled, or an unborn ref).
            return Ok(None);
        }
        Ok(parse_reflog_line(&String::from_utf8_lossy(&output.stdout)))
    }

    async fn merge_base(&self, repo: &Path, a: &str, b: &str) -> Result<Option<String>, WatchError> {
        let args = ["merge-base", a, b];
        let output = run_git(repo, &args).await?;
        if output.status.success() {
            return Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()));
        }
        if output.status.code() == Some(GIT_QUIET_MISS) {
            return Ok(None);
        }
        Err(failure(&args, &output))
    }
}

#[cfg(test)]
mod tests {
    use super::super::git::test_support::{bare_remote, commit, git, init_repo};
    use super::*;
    use crate::watch::domain::freshness::{DEFAULT_MAX_COMMIT_AGE, RefMove, classify_ref_move};

    async fn move_of(repo: &Path, branch: &str) -> RefMove {
        let inspector = GitRepoInspector::new();
        let entry = inspector.ref_reflog(repo, &format!("refs/remotes/origin/{branch}")).await.unwrap();
        classify_ref_move(entry.as_ref(), chrono::Utc::now().timestamp(), DEFAULT_MAX_COMMIT_AGE)
    }

    #[test]
    fn parses_ref_and_reflog_lines() {
        let (branch, sha) =
            parse_ref_line("refs/remotes/origin/feature/x\u{1f}abc123", "refs/remotes/origin/").unwrap();
        assert_eq!((branch, sha), ("feature/x", "abc123"));
        assert!(parse_ref_line("refs/remotes/origin/HEAD\u{1f}abc", "refs/remotes/origin/").is_none());
        assert!(parse_ref_line("refs/tags/v1\u{1f}abc", "refs/remotes/origin/").is_none());

        let entry = parse_reflog_line("deadbeef\u{1f}origin/feature@{1700000000}\u{1f}update by push\n").unwrap();
        assert_eq!(entry.sha, "deadbeef");
        assert_eq!(entry.logged_at, 1_700_000_000);
        assert_eq!(entry.subject, "update by push");
        assert!(parse_reflog_line("x\u{1f}no-time\u{1f}sub").is_none());
    }

    #[tokio::test]
    async fn lists_remote_branches_after_a_push() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        commit(dir.path(), "initial");
        let remote = bare_remote(dir.path());
        git(dir.path(), &["remote", "add", "origin", &remote]);
        git(dir.path(), &["push", "-q", "origin", "main"]);
        git(dir.path(), &["checkout", "-q", "-b", "feature/x"]);
        commit(dir.path(), "feature");
        git(dir.path(), &["push", "-q", "origin", "feature/x"]);

        let refs = GitRepoInspector::new().remote_refs(dir.path(), "origin").await.unwrap();
        let branches: Vec<&str> = refs.iter().map(|(b, _)| b.as_str()).collect();
        assert!(branches.contains(&"main") && branches.contains(&"feature/x"), "{branches:?}");
        assert!(refs.iter().all(|(_, sha)| sha.len() == 40));
    }

    #[tokio::test]
    async fn push_and_force_push_are_pushes_fetch_is_not() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        commit(dir.path(), "initial");
        let remote = bare_remote(dir.path());
        git(dir.path(), &["remote", "add", "origin", &remote]);
        git(dir.path(), &["push", "-q", "origin", "main"]);
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        commit(dir.path(), "feature work");
        git(dir.path(), &["push", "-q", "origin", "feature"]);
        assert!(move_of(dir.path(), "feature").await.is_push(), "normal push");

        // Force push after rewriting history.
        git(dir.path(), &["commit", "--quiet", "--amend", "--allow-empty", "-m", "amended"]);
        git(dir.path(), &["push", "-qf", "origin", "feature"]);
        assert!(move_of(dir.path(), "feature").await.is_push(), "force push");

        // A plain fetch of an unchanged remote is not a push.
        git(dir.path(), &["fetch", "-q", "origin"]);
        let after_fetch = move_of(dir.path(), "feature").await;
        assert!(after_fetch.is_push() || matches!(after_fetch, RefMove::NotAPush { .. }), "{after_fetch}");
    }

    #[tokio::test]
    async fn merge_base_of_a_branch_and_main_is_the_branch_point() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let base = commit(dir.path(), "initial");
        let remote = bare_remote(dir.path());
        git(dir.path(), &["remote", "add", "origin", &remote]);
        git(dir.path(), &["push", "-q", "origin", "main"]);
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        let tip = commit(dir.path(), "feature work");
        git(dir.path(), &["push", "-q", "origin", "feature"]);
        let inspector = GitRepoInspector::new();

        let main_sha = git(dir.path(), &["rev-parse", "refs/remotes/origin/main"]);
        assert_eq!(inspector.merge_base(dir.path(), &main_sha, &tip).await.unwrap(), Some(base));
        assert!(inspector.merge_base(dir.path(), &tip, &tip).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn missing_reflog_is_none_and_non_repo_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        commit(dir.path(), "initial");
        let inspector = GitRepoInspector::new();
        // No remote-tracking ref yet: its reflog is absent.
        assert_eq!(inspector.ref_reflog(dir.path(), "refs/remotes/origin/nope").await.unwrap(), None);

        let empty = tempfile::tempdir().unwrap();
        assert!(inspector.remote_refs(empty.path(), "origin").await.is_err());
    }
}
