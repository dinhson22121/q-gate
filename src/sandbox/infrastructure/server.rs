//! The runner daemon (`qgate runner`): accepts requests on a Unix socket and runs them through
//! [`RunConfined`], at most `max_jobs` at a time. A client that disconnects cancels its job (the
//! process group is killed).

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;

use super::wire::{MAX_REQUEST_BYTES, RunnerRequest, RunnerResponse, read_message, write_message};
use crate::sandbox::application::RunConfined;

/// Time a client has to send its request after connecting.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Open connections (queued + running) per job slot; further clients wait in the accept backlog.
const CONNECTIONS_PER_JOB: usize = 8;

pub struct RunnerServer {
    use_case: Arc<RunConfined>,
    jobs: Arc<Semaphore>,
    connections: Arc<Semaphore>,
}

impl RunnerServer {
    pub fn new(use_case: Arc<RunConfined>, max_jobs: usize) -> Self {
        let max_jobs = max_jobs.max(1);
        Self {
            use_case,
            jobs: Arc::new(Semaphore::new(max_jobs)),
            connections: Arc::new(Semaphore::new(max_jobs * CONNECTIONS_PER_JOB)),
        }
    }

    /// Serves until `shutdown` completes. Running jobs are cancelled (killed) when the server stops.
    pub async fn serve(&self, listener: UnixListener, shutdown: impl Future<Output = ()>) -> Result<()> {
        tokio::pin!(shutdown);
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let permit = tokio::select! {
                () = &mut shutdown => break,
                permit = self.connections.clone().acquire_owned() => permit.context("connection limit closed")?,
            };
            let stream = tokio::select! {
                () = &mut shutdown => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        eprintln!("qgate runner: accept failed: {e}");
                        continue;
                    }
                },
            };
            let (use_case, jobs) = (self.use_case.clone(), self.jobs.clone());
            connections.spawn(async move {
                if let Err(e) = handle(stream, &use_case, &jobs).await {
                    eprintln!("qgate runner: connection error: {e}");
                }
                drop(permit);
            });
            // Reap finished connection tasks so the set does not grow.
            while connections.try_join_next().is_some() {}
        }
        connections.shutdown().await;
        Ok(())
    }
}

async fn handle(stream: UnixStream, use_case: &RunConfined, jobs: &Semaphore) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let request = match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_message(&mut reader, MAX_REQUEST_BYTES)).await {
        Err(_) => return Ok(()),
        Ok(Ok(None)) => return Ok(()),
        Ok(Ok(Some(request))) => request,
        Ok(Err(e)) => return respond(&mut write_half, &RunnerResponse::Invalid { reason: e.to_string() }).await,
    };
    let request = match request {
        RunnerRequest::Ping => return respond(&mut write_half, &RunnerResponse::Pong).await,
        RunnerRequest::Exec { request } => request,
    };
    // Anything the client does from now on (closing, writing) ends the job: dropping it kills the group.
    let mut probe = [0u8; 1];
    let job = async {
        let _permit = jobs.acquire().await.ok();
        use_case.execute(request).await
    };
    tokio::select! {
        result = job => respond(&mut write_half, &RunnerResponse::Done { result }).await,
        _ = reader.read(&mut probe) => Ok(()),
    }
}

async fn respond(writer: &mut OwnedWriteHalf, response: &RunnerResponse) -> std::io::Result<()> {
    write_message(writer, response).await
}

/// Binds the runner socket: creates the parent directory, replaces a stale socket file (never any
/// other kind of file) and restricts the socket to the owner.
pub fn bind_socket(path: &Path) -> Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(path).with_context(|| format!("cannot remove stale socket {}", path.display()))?;
        }
        Ok(_) => bail!("{} exists and is not a socket; refusing to replace it", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot inspect {}", path.display())),
    }
    let listener = UnixListener::bind(path).with_context(|| format!("cannot listen on {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {}", path.display()))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Instant;

    use tokio::sync::oneshot;

    use super::*;
    use crate::sandbox::application::{ExecBackend, ExecRequest, FailureKind};
    use crate::sandbox::infrastructure::{LocalBackend, RemoteBackend};

    /// A runner on a temp socket with `work/` as its root, stopped when dropped.
    struct TestRunner {
        dir: tempfile::TempDir,
        socket: PathBuf,
        stop: Option<oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl TestRunner {
        async fn start(max_jobs: usize) -> Self {
            // Short path: Unix socket paths are limited to ~100 bytes (macOS temp dirs are long).
            let dir = tempfile::Builder::new().prefix("qr").tempdir_in("/tmp").unwrap();
            std::fs::create_dir(dir.path().join("work")).unwrap();
            let socket = dir.path().join("work/.runner/s.sock");
            let use_case = Arc::new(RunConfined::new(Arc::new(LocalBackend), &dir.path().join("work")).unwrap());
            let listener = bind_socket(&socket).unwrap();
            let (stop, stopped) = oneshot::channel::<()>();
            let server = RunnerServer::new(use_case, max_jobs);
            let task = tokio::spawn(async move {
                server
                    .serve(listener, async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
            Self { dir, socket, stop: Some(stop), task }
        }

        fn snapshot(&self, name: &str) -> PathBuf {
            let path = self.dir.path().join("work").join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        fn client(&self) -> RemoteBackend {
            RemoteBackend::new(&self.socket)
        }

        async fn stop(mut self) {
            let _ = self.stop.take().unwrap().send(());
            (&mut self.task).await.unwrap();
        }
    }

    fn request(cwd: &Path, script: &str, timeout_ms: u64) -> ExecRequest {
        ExecRequest {
            argv: vec!["sh".into(), "-c".into(), script.into()],
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms,
            max_output_bytes: 4096,
        }
    }

    fn kind(result: &crate::sandbox::application::ExecResult) -> Option<FailureKind> {
        result.failure.as_ref().map(|f| f.kind)
    }

    #[tokio::test]
    async fn runs_commands_and_answers_pings() {
        let runner = TestRunner::start(2).await;
        let client = runner.client();
        client.ping().await.unwrap();
        let mut req = request(&runner.snapshot("s1"), "pwd; echo $GIVEN; exit 4", 10_000);
        req.env.insert("GIVEN".into(), "hello".into());
        let result = client.execute(req).await;
        assert_eq!(result.exit_code, Some(4), "{result:?}");
        assert!(result.output_tail.contains("s1") && result.output_tail.contains("hello"));
        assert!(result.duration_ms < 10_000);
        runner.stop().await;
    }

    #[tokio::test]
    async fn cwd_outside_the_work_root_is_rejected() {
        let runner = TestRunner::start(1).await;
        let outside = runner.dir.path().to_path_buf();
        let result = runner.client().execute(request(&outside, "touch escaped", 10_000)).await;
        assert_eq!(kind(&result), Some(FailureKind::Rejected));
        assert!(!outside.join("escaped").exists());
        let escape = runner.snapshot("s1").join("../..");
        let result = runner.client().execute(request(&escape, "true", 10_000)).await;
        assert_eq!(kind(&result), Some(FailureKind::Rejected));
        runner.stop().await;
    }

    #[tokio::test]
    async fn timeout_is_reported_and_kills_the_group() {
        let runner = TestRunner::start(1).await;
        let cwd = runner.snapshot("s1");
        let script = "(sleep 2; touch survived) & sleep 100";
        let result = runner.client().execute(request(&cwd, script, 500)).await;
        assert!(result.timed_out && result.exit_code.is_none());
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!cwd.join("survived").exists());
        runner.stop().await;
    }

    #[tokio::test]
    async fn a_disconnecting_client_cancels_its_job() {
        let runner = TestRunner::start(1).await;
        let cwd = runner.snapshot("s1");
        let client = runner.client();
        let script = "echo $$ > pid; sleep 3; touch survived";
        let cancelled =
            tokio::time::timeout(Duration::from_secs(1), client.execute(request(&cwd, script, 60_000))).await;
        assert!(cancelled.is_err());
        tokio::time::sleep(Duration::from_millis(3500)).await;
        assert!(cwd.join("pid").exists(), "the job had started");
        assert!(!cwd.join("survived").exists(), "the job was killed when the client went away");
        runner.stop().await;
    }

    #[tokio::test]
    async fn concurrent_jobs_are_bounded() {
        let runner = TestRunner::start(2).await;
        let cwd = runner.snapshot("s1");
        let client = runner.client();
        let started = Instant::now();
        let runs = (0..4).map(|_| client.execute(request(&cwd, "sleep 0.5", 10_000)));
        let results = futures::future::join_all(runs).await;
        assert!(results.iter().all(|r| r.success()), "{results:?}");
        assert!(started.elapsed() >= Duration::from_millis(1000), "4 jobs on 2 slots take two rounds");
        runner.stop().await;
    }

    #[tokio::test]
    async fn malformed_requests_get_an_invalid_answer() {
        use tokio::io::AsyncWriteExt;
        let runner = TestRunner::start(1).await;
        let mut stream = UnixStream::connect(&runner.socket).await.unwrap();
        stream.write_all(b"{\"op\":\"shell\",\"cmd\":\"id\"}\n").await.unwrap();
        let mut reader = BufReader::new(stream);
        let answer: RunnerResponse = read_message(&mut reader, 1024).await.unwrap().unwrap();
        assert!(matches!(answer, RunnerResponse::Invalid { .. }));
        runner.stop().await;
    }

    #[tokio::test]
    async fn unreachable_runner_is_unavailable_and_nothing_runs() {
        let dir = tempfile::Builder::new().prefix("qr").tempdir_in("/tmp").unwrap();
        let client = RemoteBackend::new(dir.path().join("missing.sock"));
        let result = client.execute(request(dir.path(), "touch ran", 10_000)).await;
        assert_eq!(kind(&result), Some(FailureKind::Unavailable));
        assert!(result.failure.unwrap().message.contains("qgate runner unavailable"));
        assert!(!dir.path().join("ran").exists());
        assert!(client.ping().await.is_err());
    }

    #[tokio::test]
    async fn bind_replaces_only_stale_sockets() {
        let dir = tempfile::Builder::new().prefix("qr").tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("a/s.sock");
        drop(bind_socket(&socket).unwrap());
        assert!(bind_socket(&socket).is_ok(), "a stale socket is replaced");
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(bind_socket(&file).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "x");
    }
}
