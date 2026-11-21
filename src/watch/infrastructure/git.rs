//! Minimal runner for the `git` executable (argv only, never a shell).

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::process::Command;

use crate::watch::application::ports::WatchError;

const GIT: &str = "git";
/// A local git query that takes longer than this is considered hung.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Exit status of `--quiet`/`--verify` queries that found nothing.
pub(super) const GIT_QUIET_MISS: i32 = 1;
/// Watched repositories are untrusted (mounted with `safe.directory '*'`): their config must not get to
/// run programs (fsmonitor, hooks) or open transports.
const HARDENING_ARGS: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "protocol.allow=never",
    "-c",
    "core.sshCommand=false",
];
/// Read-only polling must never contend for the index lock with the user's git, prompt or page.
/// `GIT_CONFIG_NOSYSTEM` is deliberately not set: the Docker image relies on the system `safe.directory`.
const HARDENING_ENV: &[(&str, &str)] =
    &[("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0"), ("GIT_PAGER", "cat")];

/// Runs `git <args>` in `cwd` and returns the raw output (any exit status).
pub(super) async fn run_git(cwd: &Path, args: &[&str]) -> Result<Output, WatchError> {
    let mut command = Command::new(GIT);
    command
        .args(HARDENING_ARGS)
        .args(args)
        .current_dir(cwd)
        .envs(HARDENING_ENV.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_TIMEOUT, command.output())
        .await
        .map_err(|_| WatchError::Git(format!("git {} timed out in {}", args.join(" "), cwd.display())))?
        .map_err(|e| WatchError::Git(format!("cannot run git: {e}")))?;
    Ok(output)
}

pub(super) fn failure(args: &[&str], output: &Output) -> WatchError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    WatchError::Git(format!("git {} failed: {}", args.join(" "), stderr.trim()))
}

/// Canonical working-tree root of the repository containing `dir`, if any.
pub(super) async fn repo_root(dir: &Path) -> Result<Option<PathBuf>, WatchError> {
    let output = run_git(dir, &["rev-parse", "--show-toplevel"]).await?;
    if !output.status.success() {
        return Ok(None);
    }
    let top = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if top.is_empty() {
        return Ok(None);
    }
    Ok(Some(tokio::fs::canonicalize(&top).await?))
}

#[cfg(test)]
mod tests {
    use super::test_support::{commit, git, init_repo};
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn repository_fsmonitor_is_never_run() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        commit(dir.path(), "initial");
        let marker = dir.path().join("fsmonitor-ran");
        git(dir.path(), &["config", "core.fsmonitor", &format!("touch '{}'", marker.display())]);
        std::fs::write(dir.path().join("a.txt"), "b").unwrap();

        // `status` would consult the fsmonitor; HEAD inspection must not run it either.
        assert!(run_git(dir.path(), &["status", "--porcelain"]).await.unwrap().status.success());
        assert!(run_git(dir.path(), &["rev-parse", "HEAD"]).await.unwrap().status.success());
        assert!(repo_root(dir.path()).await.unwrap().is_some());
        assert!(!marker.exists(), "fsmonitor command from the repository config was executed");
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Temporary git repositories isolated from the user's git configuration.

    use std::path::Path;
    use std::process::Command;

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    pub fn init_repo(dir: &Path) {
        git(dir, &["init", "--quiet"]);
    }

    /// Creates a bare repository (inside `work`, so it is unique per test) to act as its `origin`,
    /// and returns its path. It is never committed (commits here are `--allow-empty`, adding no files).
    pub fn bare_remote(work: &Path) -> String {
        let remote = work.join(".origin.git");
        git(work, &["init", "--quiet", "--bare", remote.to_str().expect("utf-8 path")]);
        remote.to_string_lossy().into_owned()
    }

    pub fn commit(dir: &Path, message: &str) -> String {
        git(dir, &["commit", "--quiet", "--allow-empty", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }
}
