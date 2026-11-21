//! `qgate runner`: the isolated runner daemon (Docker service `qgate-runner`), and its health check.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::args::RunnerArgs;
use super::serve::wait_for_termination;
use super::{EXIT_ERROR, EXIT_OK};
use crate::sandbox::application::RunConfined;
use crate::sandbox::infrastructure::{ENV_WORK_DIR, LocalBackend, RemoteBackend, RunnerServer, bind_socket};

const DEFAULT_WORK_ROOT: &str = "/work";

pub async fn runner(args: &RunnerArgs) -> Result<ExitCode> {
    if args.check {
        return Ok(match RemoteBackend::new(&args.socket).ping().await {
            Ok(()) => ExitCode::from(EXIT_OK),
            Err(e) => {
                eprintln!("qgate runner: {e}");
                ExitCode::from(EXIT_ERROR)
            }
        });
    }
    let root = work_root(args.work_root.as_deref(), |name| std::env::var(name).ok());
    let use_case = RunConfined::new(Arc::new(LocalBackend), &root)
        .with_context(|| format!("work root {} does not exist", root.display()))?;
    let listener = bind_socket(&args.socket)?;
    eprintln!(
        "qgate runner: listening on {} (work root {}, {} jobs at a time)",
        args.socket.display(),
        use_case.root().display(),
        args.max_jobs.max(1)
    );
    let served = RunnerServer::new(Arc::new(use_case), args.max_jobs).serve(listener, wait_for_termination()).await;
    // Best effort: a stale socket is replaced on the next start anyway.
    let _ = std::fs::remove_file(&args.socket);
    served.map(|()| ExitCode::from(EXIT_OK))
}

fn work_root(flag: Option<&Path>, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    flag.map(Path::to_path_buf)
        .or_else(|| env(ENV_WORK_DIR).filter(|v| !v.trim().is_empty()).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WORK_ROOT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_root_prefers_flag_then_env_then_default() {
        let env = |value: &'static str| move |_: &str| Some(value.to_string());
        assert_eq!(work_root(Some(Path::new("/w")), env("/e")), PathBuf::from("/w"));
        assert_eq!(work_root(None, env("/e")), PathBuf::from("/e"));
        assert_eq!(work_root(None, env(" ")), PathBuf::from(DEFAULT_WORK_ROOT));
        assert_eq!(work_root(None, |_: &str| None), PathBuf::from(DEFAULT_WORK_ROOT));
    }

    #[tokio::test]
    async fn check_fails_without_a_runner_and_serving_needs_the_work_root() {
        let dir = tempfile::Builder::new().prefix("qr").tempdir_in("/tmp").unwrap();
        let args = RunnerArgs { socket: dir.path().join("s.sock"), work_root: None, max_jobs: 1, check: true };
        assert_eq!(runner(&args).await.unwrap(), ExitCode::from(EXIT_ERROR));
        let args = RunnerArgs { work_root: Some(dir.path().join("missing")), check: false, ..args };
        assert!(runner(&args).await.unwrap_err().to_string().contains("does not exist"));
    }
}
