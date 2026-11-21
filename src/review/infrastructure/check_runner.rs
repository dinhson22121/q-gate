use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};

use crate::review::application::ports::CheckRunner;
use crate::review::domain::checks::{CheckOutcome, CheckResult, CheckSpec};
use crate::sandbox::application::{ExecBackend, ExecRequest, ExecResult};

pub const DEFAULT_MAX_PARALLEL: usize = 2;
const TAIL_MAX_LINES: usize = 40;
const TAIL_MAX_BYTES: usize = 8 * 1024;
const MILLIS_PER_SEC: u64 = 1000;

#[derive(Clone)]
pub struct ProcessCheckRunner {
    root: PathBuf,
    max_parallel: usize,
    backend: Arc<dyn ExecBackend>,
}

impl ProcessCheckRunner {
    pub fn new(root: impl Into<PathBuf>, backend: Arc<dyn ExecBackend>) -> Self {
        Self { root: root.into(), max_parallel: DEFAULT_MAX_PARALLEL, backend }
    }

    pub fn with_max_parallel(mut self, max_parallel: usize) -> Self {
        self.max_parallel = max_parallel.max(1);
        self
    }

    async fn run_one(&self, spec: &CheckSpec) -> CheckResult {
        let started = Instant::now();
        let (outcome, exit_code, output) = match spec.command.first() {
            Some(program) if !program.is_empty() => self.execute(spec).await,
            _ => (CheckOutcome::Error, None, "empty command".to_string()),
        };
        CheckResult {
            name: spec.name.clone(),
            required: spec.required,
            outcome,
            exit_code,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            output_tail: output,
        }
    }

    async fn execute(&self, spec: &CheckSpec) -> (CheckOutcome, Option<i32>, String) {
        let request = ExecRequest {
            argv: spec.command.clone(),
            cwd: self.root.clone(),
            env: Default::default(),
            timeout_ms: spec.timeout_secs.max(1).saturating_mul(MILLIS_PER_SEC),
            max_output_bytes: TAIL_MAX_BYTES,
        };
        outcome_of(self.backend.execute(request).await, spec.timeout_secs)
    }
}

impl CheckRunner for ProcessCheckRunner {
    async fn run(&self, checks: &[CheckSpec]) -> Vec<CheckResult> {
        let runs: Vec<_> = checks.iter().map(|spec| self.run_one(spec)).collect();
        stream::iter(runs).buffered(self.max_parallel).collect().await
    }
}

fn outcome_of(result: ExecResult, timeout_secs: u64) -> (CheckOutcome, Option<i32>, String) {
    let tail = last_lines(&result.output_tail);
    if let Some(failure) = result.failure {
        return (CheckOutcome::Error, None, append_line(tail, &failure.message));
    }
    if result.timed_out {
        return (CheckOutcome::TimedOut, None, append_line(tail, &format!("timed out after {timeout_secs}s")));
    }
    let outcome = if result.exit_code == Some(0) { CheckOutcome::Passed } else { CheckOutcome::Failed };
    (outcome, result.exit_code, tail)
}

fn last_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(TAIL_MAX_LINES);
    lines[start..].join("\n")
}

fn append_line(mut text: String, line: &str) -> String {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::sandbox::application::FailureKind;
    use crate::sandbox::application::fakes::RecordingBackend;
    use crate::sandbox::infrastructure::LocalBackend;

    fn spec(name: &str, command: &[&str], timeout_secs: u64) -> CheckSpec {
        CheckSpec {
            name: name.into(),
            command: command.iter().map(|s| s.to_string()).collect(),
            required: true,
            timeout_secs,
        }
    }

    fn local(root: impl Into<PathBuf>) -> ProcessCheckRunner {
        ProcessCheckRunner::new(root, Arc::new(LocalBackend))
    }

    fn runner() -> ProcessCheckRunner {
        local(std::env::temp_dir())
    }

    async fn run(spec: CheckSpec) -> CheckResult {
        runner().run(&[spec]).await.remove(0)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn passing_check() {
        let result = run(spec("ok", &["sh", "-c", "echo hello; exit 0"], 10)).await;
        assert_eq!(result.outcome, CheckOutcome::Passed);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.output_tail, "hello");
        assert!(!result.is_blocking());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failing_check_captures_stdout_and_stderr() {
        let result = run(spec("bad", &["sh", "-c", "echo out; echo err >&2; exit 3"], 10)).await;
        assert_eq!(result.outcome, CheckOutcome::Failed);
        assert_eq!(result.exit_code, Some(3));
        assert!(result.output_tail.contains("out") && result.output_tail.contains("err"), "{}", result.output_tail);
        assert!(result.is_blocking());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_in_the_root_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "").unwrap();
        let results = local(dir.path()).run(&[spec("ls", &["ls"], 10)]).await;
        assert!(results[0].output_tail.contains("marker.txt"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_process() {
        let started = Instant::now();
        let result = run(spec("slow", &["sleep", "30"], 1)).await;
        assert_eq!(result.outcome, CheckOutcome::TimedOut);
        assert_eq!(result.exit_code, None);
        assert!(result.output_tail.contains("timed out after 1s"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn missing_program_is_an_error() {
        let result = run(spec("missing", &["qgate-definitely-not-a-program"], 10)).await;
        assert_eq!(result.outcome, CheckOutcome::Error);
        assert!(result.output_tail.contains("failed to start 'qgate-definitely-not-a-program'"));
    }

    #[tokio::test]
    async fn empty_command_is_an_error() {
        for command in [&[][..], &[""][..]] {
            let result = run(spec("empty", command, 10)).await;
            assert_eq!(result.outcome, CheckOutcome::Error);
            assert_eq!(result.output_tail, "empty command");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn keeps_only_the_last_lines() {
        let result =
            run(spec("many", &["sh", "-c", "i=1; while [ $i -le 100 ]; do echo line$i; i=$((i+1)); done"], 10)).await;
        let lines: Vec<&str> = result.output_tail.lines().collect();
        assert_eq!(lines.len(), TAIL_MAX_LINES);
        assert_eq!(lines.first(), Some(&"line61"));
        assert_eq!(lines.last(), Some(&"line100"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn keeps_only_the_last_bytes() {
        let script = "head -c 50000 /dev/zero | tr '\\0' 'a'; echo; echo end";
        let result = run(spec("wide", &["sh", "-c", script], 10)).await;
        assert!(result.output_tail.len() <= TAIL_MAX_BYTES);
        assert_eq!(result.output_tail, "end", "partial first line is dropped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn results_follow_spec_order_with_bounded_parallelism() {
        let checks = [
            spec("a", &["sh", "-c", "sleep 0.3; echo a"], 10),
            spec("b", &["sh", "-c", "echo b"], 10),
            spec("c", &["sh", "-c", "exit 1"], 10),
        ];
        let results = runner().with_max_parallel(0).run(&checks).await;
        let names: Vec<_> = results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(results[2].outcome, CheckOutcome::Failed);
        let results = runner().with_max_parallel(3).run(&checks).await;
        assert_eq!(results[0].output_tail, "a");
        assert_eq!(results[1].output_tail, "b");
    }

    #[test]
    fn append_line_separates_with_newline() {
        assert_eq!(append_line(String::new(), "x"), "x");
        assert_eq!(append_line("a".into(), "x"), "a\nx");
        assert_eq!(append_line("a\n".into(), "x"), "a\nx");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn secrets_in_the_environment_are_not_passed_to_checks() {
        // SAFETY: test-only; no other thread in this test reads this variable concurrently.
        unsafe { std::env::set_var("QGATE_TEST_SECRET_SHOULD_NOT_LEAK", "s3cret") };
        let dir = tempfile::tempdir().unwrap();
        let results = local(dir.path()).run(&[spec("env", &["sh", "-c", "env"], 10)]).await;
        assert_eq!(results[0].outcome, CheckOutcome::Passed);
        assert!(!results[0].output_tail.contains("s3cret"));
        assert!(results[0].output_tail.contains("PATH="));
    }

    #[tokio::test]
    async fn requests_carry_root_timeout_and_output_cap() {
        let backend = Arc::new(RecordingBackend::answering(ExecResult { exit_code: Some(0), ..ExecResult::default() }));
        let runner = ProcessCheckRunner::new("/work/snap", backend.clone());
        let results = runner.run(&[spec("go test", &["go", "test", "./..."], 90)]).await;
        assert_eq!(results[0].outcome, CheckOutcome::Passed);
        let sent = backend.requests.lock().unwrap()[0].clone();
        assert_eq!(sent.argv, ["go", "test", "./..."]);
        assert_eq!(sent.cwd, PathBuf::from("/work/snap"));
        assert_eq!(sent.timeout_ms, 90_000);
        assert_eq!(sent.max_output_bytes, TAIL_MAX_BYTES);
        assert!(sent.env.is_empty());
    }

    #[tokio::test]
    async fn unreachable_runner_fails_the_check_with_a_note() {
        let down =
            ExecResult::failed(FailureKind::Unavailable, "qgate runner unavailable at /work/.runner/runner.sock");
        let runner = ProcessCheckRunner::new("/work/snap", Arc::new(RecordingBackend::answering(down)));
        let result = runner.run(&[spec("go test", &["go", "test"], 10)]).await.remove(0);
        assert_eq!(result.outcome, CheckOutcome::Error);
        assert!(result.is_blocking());
        assert!(result.output_tail.contains("qgate runner unavailable"));
    }

    #[test]
    fn signal_deaths_fail_and_timeouts_keep_the_tail() {
        let killed = ExecResult { exit_code: None, output_tail: "boom\n".into(), ..ExecResult::default() };
        assert_eq!(outcome_of(killed, 5), (CheckOutcome::Failed, None, "boom".into()));
        let slow = ExecResult { timed_out: true, output_tail: "partial".into(), ..ExecResult::default() };
        assert_eq!(outcome_of(slow, 5), (CheckOutcome::TimedOut, None, "partial\ntimed out after 5s".into()));
    }
}
