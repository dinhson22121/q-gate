use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::process::Command;

use crate::watch::application::ports::WatchError;

const GIT: &str = "git";
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const GIT_QUIET_MISS: i32 = 1;
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
const HARDENING_ENV: &[(&str, &str)] =
    &[("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0"), ("GIT_PAGER", "cat")];

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

        assert!(run_git(dir.path(), &["status", "--porcelain"]).await.unwrap().status.success());
        assert!(run_git(dir.path(), &["rev-parse", "HEAD"]).await.unwrap().status.success());
        assert!(repo_root(dir.path()).await.unwrap().is_some());
        assert!(!marker.exists(), "fsmonitor command from the repository config was executed");
    }
}

#[cfg(test)]
pub(crate) mod test_support {

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
