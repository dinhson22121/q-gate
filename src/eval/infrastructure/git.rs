use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::eval::application::EvalError;

const GIT: &str = "git";
const IDENTITY_ARGS: &[&str] = &[
    "-c",
    "user.name=qgate-eval",
    "-c",
    "user.email=eval@qgate.invalid",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.allow=never",
];
const ISOLATION_ENV: &[(&str, &str)] = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_PAGER", "cat"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
];

pub(super) fn safe_directory_args(repo: &Path) -> Vec<String> {
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    vec!["-c".to_string(), format!("safe.directory={}", canonical.display())]
}

pub(super) async fn git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, EvalError> {
    run_git(cwd, &[], args).await
}

pub(super) async fn user_repo_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>, EvalError> {
    run_git(repo, &safe_directory_args(repo), args).await
}

pub(super) async fn user_repo_text(repo: &Path, args: &[&str]) -> Result<String, EvalError> {
    let bytes = user_repo_bytes(repo, args).await?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

async fn run_git(cwd: &Path, extra_config: &[String], args: &[&str]) -> Result<Vec<u8>, EvalError> {
    let output = Command::new(GIT)
        .args(IDENTITY_ARGS)
        .args(extra_config)
        .args(args)
        .envs(ISOLATION_ENV.iter().copied())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| EvalError::Workspace(format!("cannot run git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EvalError::Workspace(format!("git {} failed: {}", args.join(" "), stderr.trim())));
    }
    Ok(output.stdout)
}

pub(super) async fn git_text(cwd: &Path, args: &[&str]) -> Result<String, EvalError> {
    let bytes = git_bytes(cwd, args).await?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

pub(super) fn is_safe_rev(rev: &str) -> bool {
    !rev.is_empty()
        && !rev.starts_with('-')
        && rev.len() <= 200
        && rev
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && !matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
}
