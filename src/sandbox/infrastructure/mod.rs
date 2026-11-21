mod local;
#[cfg(unix)]
mod remote;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod wire;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use local::LocalBackend;
#[cfg(unix)]
pub use remote::RemoteBackend;
#[cfg(unix)]
pub use server::{RunnerServer, bind_socket};

use crate::sandbox::application::ExecBackend;

pub const ENV_RUNNER_SOCKET: &str = "QGATE_RUNNER_SOCKET";
pub const ENV_WORK_DIR: &str = "QGATE_WORK_DIR";
const DEFAULT_WORK_SUBDIR: &str = "work";

pub fn backend_from_env(env: impl Fn(&str) -> Option<String>) -> Arc<dyn ExecBackend> {
    match runner_socket(env) {
        Some(socket) => remote_backend(socket),
        None => Arc::new(LocalBackend),
    }
}

pub fn runner_socket(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env(ENV_RUNNER_SOCKET).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn work_dir(env: impl Fn(&str) -> Option<String>, data_dir: &Path) -> PathBuf {
    env(ENV_WORK_DIR)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map_or_else(|| data_dir.join(DEFAULT_WORK_SUBDIR), PathBuf::from)
}

#[cfg(unix)]
fn remote_backend(socket: PathBuf) -> Arc<dyn ExecBackend> {
    Arc::new(RemoteBackend::new(socket))
}

#[cfg(not(unix))]
fn remote_backend(_socket: PathBuf) -> Arc<dyn ExecBackend> {
    use futures::future::BoxFuture;

    use crate::sandbox::application::{ExecRequest, ExecResult, FailureKind};

    struct Unsupported;
    impl ExecBackend for Unsupported {
        fn execute(&self, _request: ExecRequest) -> BoxFuture<'_, ExecResult> {
            Box::pin(async {
                ExecResult::failed(FailureKind::Unavailable, "QGATE_RUNNER_SOCKET is only supported on Unix")
            })
        }
    }
    Arc::new(Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| (*v).to_string())
    }

    #[test]
    fn runner_socket_and_work_dir_come_from_the_environment() {
        assert_eq!(runner_socket(env(&[(ENV_RUNNER_SOCKET, " /work/r.sock ")])), Some("/work/r.sock".into()));
        assert_eq!(runner_socket(env(&[(ENV_RUNNER_SOCKET, "  ")])), None);
        assert_eq!(runner_socket(env(&[])), None);
        assert_eq!(work_dir(env(&[(ENV_WORK_DIR, "/work")]), Path::new("/data")), PathBuf::from("/work"));
        assert_eq!(work_dir(env(&[]), Path::new("/data")), PathBuf::from("/data/work"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_socket_variable_selects_the_remote_backend_without_fallback() {
        use crate::sandbox::application::{ExecRequest, FailureKind};
        let dir = tempfile::tempdir().unwrap();
        let request = ExecRequest {
            argv: vec!["sh".into(), "-c".into(), "touch ran".into()],
            cwd: dir.path().to_path_buf(),
            env: Default::default(),
            timeout_ms: 5_000,
            max_output_bytes: 100,
        };
        let remote = backend_from_env(env(&[(ENV_RUNNER_SOCKET, "/nonexistent/qgate-runner.sock")]));
        let result = remote.execute(request.clone()).await;
        assert_eq!(result.failure.map(|f| f.kind), Some(FailureKind::Unavailable));
        assert!(!dir.path().join("ran").exists(), "never falls back to local execution");
        let local = backend_from_env(env(&[]));
        assert!(local.execute(request).await.success());
        assert!(dir.path().join("ran").exists());
    }
}
