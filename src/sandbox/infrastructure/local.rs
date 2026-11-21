use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::sandbox::application::ExecBackend;
use crate::sandbox::domain::{ExecRequest, ExecResult, FailureKind, INHERITED_ENV_VARS, OutputTail, validate};

const READ_CHUNK_BYTES: usize = 4096;
const DRAIN_GRACE: Duration = Duration::from_secs(2);
const HOMES_DROPPED_WHEN_MISSING: &[&str] = &["JAVA_HOME", "MAVEN_HOME"];
const SCRATCH_HOME_PREFIX: &str = "qgate-exec-home-";

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalBackend;

impl ExecBackend for LocalBackend {
    fn execute(&self, request: ExecRequest) -> BoxFuture<'_, ExecResult> {
        Box::pin(run(request))
    }
}

async fn run(request: ExecRequest) -> ExecResult {
    let started = Instant::now();
    let mut result = run_unmeasured(&request).await;
    result.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    result
}

async fn run_unmeasured(request: &ExecRequest) -> ExecResult {
    if let Err(reason) = validate(request) {
        return ExecResult::failed(FailureKind::Rejected, reason);
    }
    let (program, args) = request.argv.split_first().expect("validated: argv is not empty");
    let env = child_env(inherited_env(), &request.env);
    let scratch_home = match scratch_home_if_missing(&env) {
        Ok(home) => home,
        Err(e) => return ExecResult::failed(FailureKind::SpawnFailed, format!("cannot create a HOME directory: {e}")),
    };
    let mut command = Command::new(program_path(&request.cwd, program));
    command
        .args(args)
        .env_clear()
        .envs(&env)
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(home) = &scratch_home {
        command.env("HOME", home.path());
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let kind =
                if e.kind() == std::io::ErrorKind::NotFound { FailureKind::NotFound } else { FailureKind::SpawnFailed };
            return ExecResult::failed(kind, format!("failed to start '{program}': {e}"));
        }
    };
    let mut group = ProcessGroup::of(&child);
    let mut tail = OutputTail::new(request.max_output_bytes);
    let timeout = Duration::from_millis(request.timeout_ms);
    let waited = tokio::time::timeout(timeout, collect(&mut child, &mut tail)).await;
    group.kill();
    match waited {
        Ok(Ok(status)) => ExecResult { exit_code: status.code(), output_tail: tail.render(), ..ExecResult::default() },
        Ok(Err(e)) => ExecResult {
            output_tail: tail.render(),
            ..ExecResult::failed(FailureKind::SpawnFailed, format!("failed to wait for '{program}': {e}"))
        },
        Err(_) => {
            let _ = child.kill().await;
            ExecResult { timed_out: true, output_tail: tail.render(), ..ExecResult::default() }
        }
    }
}

fn program_path(cwd: &Path, program: &str) -> PathBuf {
    if program.starts_with("./") || program.starts_with("../") { cwd.join(program) } else { PathBuf::from(program) }
}

fn child_env(inherited: Vec<(String, String)>, extra: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = inherited
        .into_iter()
        .filter(|(name, value)| !HOMES_DROPPED_WHEN_MISSING.contains(&name.as_str()) || Path::new(value).is_dir())
        .collect();
    env.extend(extra.iter().map(|(name, value)| (name.clone(), value.clone())));
    env
}

fn inherited_env() -> Vec<(String, String)> {
    INHERITED_ENV_VARS
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| ((*name).to_string(), value)))
        .collect()
}

fn scratch_home_if_missing(env: &BTreeMap<String, String>) -> std::io::Result<Option<tempfile::TempDir>> {
    if env.contains_key("HOME") {
        return Ok(None);
    }
    tempfile::Builder::new().prefix(SCRATCH_HOME_PREFIX).tempdir().map(Some)
}

struct ProcessGroup {
    #[cfg(unix)]
    pgid: Option<libc::pid_t>,
}

impl ProcessGroup {
    #[cfg(unix)]
    fn of(child: &Child) -> Self {
        Self { pgid: child.id().and_then(|id| libc::pid_t::try_from(id).ok()) }
    }

    #[cfg(not(unix))]
    fn of(_child: &Child) -> Self {
        Self {}
    }

    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            // SAFETY: plain syscall without memory arguments; `pgid` is the group created at spawn.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn collect(child: &mut Child, tail: &mut OutputTail) -> std::io::Result<ExitStatus> {
    let reading = read_streams(child.stdout.take(), child.stderr.take(), tail);
    tokio::pin!(reading);
    tokio::select! {
        () = &mut reading => child.wait().await,
        status = child.wait() => {
            let _ = tokio::time::timeout(DRAIN_GRACE, reading).await;
            status
        }
    }
}

async fn read_streams<A: AsyncRead + Unpin, B: AsyncRead + Unpin>(
    mut stdout: Option<A>,
    mut stderr: Option<B>,
    tail: &mut OutputTail,
) {
    let mut out_buf = [0u8; READ_CHUNK_BYTES];
    let mut err_buf = [0u8; READ_CHUNK_BYTES];
    while stdout.is_some() || stderr.is_some() {
        tokio::select! {
            read = read_some(&mut stdout, &mut out_buf) => consume(tail, read, &out_buf, &mut stdout),
            read = read_some(&mut stderr, &mut err_buf) => consume(tail, read, &err_buf, &mut stderr),
        }
    }
}

async fn read_some<R: AsyncRead + Unpin>(reader: &mut Option<R>, buf: &mut [u8]) -> std::io::Result<usize> {
    match reader {
        Some(reader) => reader.read(buf).await,
        None => std::future::pending().await,
    }
}

fn consume<R>(tail: &mut OutputTail, read: std::io::Result<usize>, buf: &[u8], stream: &mut Option<R>) {
    match read {
        Ok(n) if n > 0 => tail.push(&buf[..n]),
        _ => *stream = None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(cwd: &Path, argv: &[&str], timeout_ms: u64) -> ExecRequest {
        ExecRequest {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms,
            max_output_bytes: 8 * 1024,
        }
    }

    async fn exec(request: ExecRequest) -> ExecResult {
        LocalBackend.execute(request).await
    }

    #[cfg(unix)]
    async fn group_is_gone(pgid: libc::pid_t) -> bool {
        for _ in 0..50 {
            // SAFETY: signal 0 only checks for existence.
            let alive = unsafe { libc::kill(-pgid, 0) } == 0;
            if !alive && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[cfg(unix)]
    fn read_pgid(path: &Path) -> libc::pid_t {
        std::fs::read_to_string(path).unwrap().trim().parse().unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_exit_code_and_interleaved_output() {
        let dir = tempfile::tempdir().unwrap();
        let result = exec(request(dir.path(), &["sh", "-c", "echo out; echo err >&2; exit 3"], 10_000)).await;
        assert_eq!(result.exit_code, Some(3));
        assert!(result.output_tail.contains("out") && result.output_tail.contains("err"));
        assert!(!result.timed_out && result.failure.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-survived");
        let script = format!("echo $$ > pgid; (sleep 2; touch '{}') & sleep 100 & sleep 100", marker.display());
        let started = Instant::now();
        let result = exec(request(dir.path(), &["sh", "-c", &script], 1000)).await;
        assert!(result.timed_out && result.exit_code.is_none());
        assert!(started.elapsed() < Duration::from_secs(10), "did not wait for the grandchildren");
        assert!(group_is_gone(read_pgid(&dir.path().join("pgid"))).await, "a process of the group survived");
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "the background grandchild kept running");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_a_run_kills_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let run = exec(request(dir.path(), &["sh", "-c", "echo $$ > pgid; sleep 100 & sleep 100"], 60_000));
        let cancelled = tokio::time::timeout(Duration::from_secs(1), run).await;
        assert!(cancelled.is_err(), "the run was cancelled");
        assert!(group_is_gone(read_pgid(&dir.path().join("pgid"))).await, "a process of the group survived the drop");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_processes_do_not_outlive_a_finished_run() {
        let dir = tempfile::tempdir().unwrap();
        let script = "echo $$ > pgid; sleep 100 >/dev/null 2>&1 & echo done";
        let result = exec(request(dir.path(), &["sh", "-c", script], 10_000)).await;
        assert!(result.success());
        assert!(group_is_gone(read_pgid(&dir.path().join("pgid"))).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_is_capped_to_the_requested_tail() {
        let dir = tempfile::tempdir().unwrap();
        let mut req =
            request(dir.path(), &["sh", "-c", "head -c 50000 /dev/zero | tr '\\0' 'a'; echo; echo end"], 10_000);
        req.max_output_bytes = 100;
        let result = exec(req).await;
        assert_eq!(result.output_tail, "end\n", "partial first line is dropped");
    }

    #[tokio::test]
    async fn missing_program_and_invalid_requests_are_failures() {
        let dir = tempfile::tempdir().unwrap();
        let result = exec(request(dir.path(), &["qgate-definitely-not-a-program"], 10_000)).await;
        let failure = result.failure.unwrap();
        assert_eq!(failure.kind, FailureKind::NotFound);
        assert!(failure.message.contains("failed to start 'qgate-definitely-not-a-program'"));
        let result = exec(request(dir.path(), &[], 10_000)).await;
        assert_eq!(result.failure.unwrap().kind, FailureKind::Rejected);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn environment_is_cleared_except_allow_list_and_request_vars() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: test-only; no other thread in this test reads this variable concurrently.
        unsafe { std::env::set_var("QGATE_SANDBOX_SECRET_SHOULD_NOT_LEAK", "s3cret") };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mvnw");
        std::fs::write(&script, "#!/bin/sh\nenv\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut req = request(dir.path(), &["./mvnw"], 10_000);
        req.env.insert("QGATE_EXTRA".into(), "given".into());
        let result = exec(req).await;
        assert!(result.success(), "{}", result.output_tail);
        assert!(result.output_tail.contains("PATH=") && result.output_tail.contains("QGATE_EXTRA=given"));
        assert!(!result.output_tail.contains("s3cret"));
    }

    #[test]
    fn helpers() {
        assert_eq!(program_path(Path::new("/w"), "go"), PathBuf::from("go"));
        assert_eq!(program_path(Path::new("/w"), "../gradlew"), PathBuf::from("/w/../gradlew"));
        let inherited = vec![
            ("JAVA_HOME".to_string(), "/definitely/missing/java".to_string()),
            ("MAVEN_HOME".to_string(), std::env::temp_dir().display().to_string()),
            ("PATH".to_string(), "/bin".to_string()),
        ];
        let extra = BTreeMap::from([("PATH".to_string(), "/override".to_string())]);
        let env = child_env(inherited, &extra);
        assert!(!env.contains_key("JAVA_HOME") && env.contains_key("MAVEN_HOME"));
        assert_eq!(env["PATH"], "/override");
        assert!(scratch_home_if_missing(&BTreeMap::from([("HOME".into(), "/h".into())])).unwrap().is_none());
        assert!(scratch_home_if_missing(&BTreeMap::new()).unwrap().unwrap().path().is_dir());
    }
}
