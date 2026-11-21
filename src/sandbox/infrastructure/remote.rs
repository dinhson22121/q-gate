use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::io::BufReader;
use tokio::net::UnixStream;

use super::wire::{MAX_RESPONSE_BYTES, RunnerRequest, RunnerResponse, read_message, write_message};
use crate::sandbox::application::ExecBackend;
use crate::sandbox::domain::{ExecRequest, ExecResult, FailureKind};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_GRACE: Duration = Duration::from_secs(120);
const PING_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct RemoteBackend {
    socket: PathBuf,
}

impl RemoteBackend {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self { socket: socket.into() }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    async fn call(&self, request: &RunnerRequest, wait: Duration) -> Result<RunnerResponse, String> {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(&self.socket))
            .await
            .map_err(|_| "connection timed out".to_string())?
            .map_err(|e| e.to_string())?;
        let (read_half, mut write_half) = stream.into_split();
        write_message(&mut write_half, request).await.map_err(|e| format!("cannot send the request: {e}"))?;
        let mut reader = BufReader::new(read_half);
        let response = tokio::time::timeout(wait, read_message(&mut reader, MAX_RESPONSE_BYTES))
            .await
            .map_err(|_| format!("no answer within {}s", wait.as_secs()))?
            .map_err(|e| format!("invalid answer: {e}"))?;
        drop(write_half);
        response.ok_or_else(|| "the runner closed the connection without an answer".to_string())
    }

    pub async fn ping(&self) -> Result<(), String> {
        match self.call(&RunnerRequest::Ping, PING_TIMEOUT).await {
            Ok(RunnerResponse::Pong) => Ok(()),
            Ok(other) => Err(format!("unexpected answer {other:?}")),
            Err(e) => Err(format!("runner at {}: {e}", self.socket.display())),
        }
    }

    fn unavailable(&self, reason: &str) -> ExecResult {
        ExecResult::failed(
            FailureKind::Unavailable,
            format!(
                "qgate runner unavailable at {} ({reason}); the command was not run (QGATE_RUNNER_SOCKET is set, so \
                 untrusted code never runs inside the qgate container)",
                self.socket.display()
            ),
        )
    }
}

impl ExecBackend for RemoteBackend {
    fn execute(&self, request: ExecRequest) -> BoxFuture<'_, ExecResult> {
        Box::pin(async move {
            let wait = Duration::from_millis(request.timeout_ms).saturating_add(RESPONSE_GRACE);
            match self.call(&RunnerRequest::Exec { request }, wait).await {
                Ok(RunnerResponse::Done { result }) => result,
                Ok(RunnerResponse::Invalid { reason }) => ExecResult::failed(FailureKind::Rejected, reason),
                Ok(RunnerResponse::Pong) => self.unavailable("unexpected answer"),
                Err(e) => self.unavailable(&e),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    use super::*;

    async fn fake_runner(answer: Option<&'static str>) -> (tempfile::TempDir, RemoteBackend) {
        let dir = tempfile::Builder::new().prefix("qr").tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            if let Some(answer) = answer {
                stream.get_mut().write_all(answer.as_bytes()).await.unwrap();
            }
        });
        (dir, RemoteBackend::new(socket))
    }

    fn request() -> ExecRequest {
        ExecRequest {
            argv: vec!["true".into()],
            cwd: PathBuf::from("/work/s"),
            env: BTreeMap::new(),
            timeout_ms: 1000,
            max_output_bytes: 10,
        }
    }

    fn failure(result: &ExecResult) -> (FailureKind, String) {
        let failure = result.failure.clone().expect("a failure");
        (failure.kind, failure.message)
    }

    #[tokio::test]
    async fn invalid_and_unexpected_answers_are_failures() {
        let (_dir, client) = fake_runner(Some("{\"status\":\"invalid\",\"reason\":\"bad\"}\n")).await;
        assert_eq!(failure(&client.execute(request()).await), (FailureKind::Rejected, "bad".into()));

        let (_dir, client) = fake_runner(Some("{\"status\":\"pong\"}\n")).await;
        let (kind, message) = failure(&client.execute(request()).await);
        assert_eq!(kind, FailureKind::Unavailable);
        assert!(message.contains("unexpected answer"));

        let (_dir, client) = fake_runner(Some("not json\n")).await;
        assert!(failure(&client.execute(request()).await).1.contains("invalid answer"));

        let (_dir, client) = fake_runner(None).await;
        assert!(failure(&client.execute(request()).await).1.contains("without an answer"));

        let (dir, client) = fake_runner(Some("{\"status\":\"done\",\"result\":{\"exit_code\":0,\"output_tail\":\"\",\"timed_out\":false,\"duration_ms\":1}}\n")).await;
        assert!(client.ping().await.unwrap_err().contains("unexpected answer"));
        assert_eq!(client.socket(), dir.path().join("s.sock"));
    }
}
