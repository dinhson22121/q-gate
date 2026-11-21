use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::sandbox::application::{ExecBackend, ExecRequest, ExecResult, FailureKind};
use crate::testing::application::ports::TestRunner;
use crate::testing::domain::classify::RawRun;

const TAIL_MAX_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct ExecTestRunner {
    backend: Arc<dyn ExecBackend>,
}

impl ExecTestRunner {
    pub fn new(backend: Arc<dyn ExecBackend>) -> Self {
        Self { backend }
    }
}

impl TestRunner for ExecTestRunner {
    async fn run(&self, cwd: &Path, command: &[String], timeout: Duration) -> RawRun {
        if command.is_empty() {
            return RawRun { spawn_error: Some("empty command".into()), ..RawRun::default() };
        }
        let request = ExecRequest {
            argv: command.to_vec(),
            cwd: cwd.to_path_buf(),
            env: Default::default(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX).max(1),
            max_output_bytes: TAIL_MAX_BYTES,
        };
        raw_run(self.backend.execute(request).await, timeout)
    }
}

fn raw_run(result: ExecResult, timeout: Duration) -> RawRun {
    let tail = result.output_tail.trim_end().to_string();
    if let Some(failure) = result.failure {
        return RawRun {
            not_found: failure.kind == FailureKind::NotFound,
            output: append_line(tail, &failure.message),
            spawn_error: Some(failure.message),
            ..RawRun::default()
        };
    }
    if result.timed_out {
        let output = append_line(tail, &format!("timed out after {}s", timeout.as_secs()));
        return RawRun { timed_out: true, output, ..RawRun::default() };
    }
    RawRun { exit_code: result.exit_code, output: tail, ..RawRun::default() }
}

fn append_line(text: String, line: &str) -> String {
    if text.is_empty() { line.to_string() } else { format!("{text}\n{line}") }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::sandbox::application::fakes::RecordingBackend;
    use crate::sandbox::infrastructure::LocalBackend;

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn local() -> ExecTestRunner {
        ExecTestRunner::new(Arc::new(LocalBackend))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_exit_code_and_output() {
        let result =
            local().run(&std::env::temp_dir(), &sh("echo out; echo err >&2; exit 3"), Duration::from_secs(10)).await;
        assert_eq!(result.exit_code, Some(3));
        assert!(result.output.contains("out") && result.output.contains("err"));
        assert!(!result.timed_out && result.spawn_error.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let result = local().run(dir.path(), &sh("sleep 100"), Duration::from_secs(1)).await;
        assert!(result.timed_out && result.exit_code.is_none());
        assert!(result.output.contains("timed out after 1s"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn missing_program_is_reported_as_not_found() {
        let dir = std::env::temp_dir();
        let result = local().run(&dir, &["qgate-no-such-toolchain".to_string()], Duration::from_secs(10)).await;
        assert!(result.not_found);
        assert!(result.spawn_error.unwrap().contains("qgate-no-such-toolchain"));
        let empty = local().run(&dir, &[], Duration::from_secs(10)).await;
        assert_eq!(empty.spawn_error.as_deref(), Some("empty command"));
    }

    #[tokio::test]
    async fn unreachable_runner_is_a_spawn_error_with_the_reason() {
        let down =
            ExecResult::failed(FailureKind::Unavailable, "qgate runner unavailable at /work/.runner/runner.sock");
        let backend = Arc::new(RecordingBackend::answering(down));
        let runner = ExecTestRunner::new(backend.clone());
        let result = runner.run(Path::new("/work/s1"), &["go".into(), "test".into()], Duration::from_secs(30)).await;
        assert!(!result.not_found);
        assert!(result.spawn_error.unwrap().contains("qgate runner unavailable"));
        let sent = backend.requests.lock().unwrap()[0].clone();
        assert_eq!((sent.timeout_ms, sent.max_output_bytes), (30_000, TAIL_MAX_BYTES));
        assert_eq!(sent.cwd, Path::new("/work/s1"));
    }

    #[test]
    fn output_is_trimmed() {
        let run = raw_run(
            ExecResult { exit_code: Some(0), output_tail: "ok\n\n".into(), ..ExecResult::default() },
            Duration::ZERO,
        );
        assert_eq!(run.output, "ok");
        assert_eq!(append_line(String::new(), "x"), "x");
    }
}
