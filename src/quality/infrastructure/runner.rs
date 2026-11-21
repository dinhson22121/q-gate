//! Runs one tool through the sandbox and turns every way it can go wrong into a note.

use std::path::Path;
use std::sync::Arc;

use crate::sandbox::application::{ExecBackend, ExecRequest, ExecResult, MAX_OUTPUT_BYTES};

const MILLIS_PER_SEC: u64 = 1000;
/// Last output lines quoted in a failure note.
const NOTE_LINES: usize = 3;

#[derive(Clone)]
pub struct ToolRunner {
    backend: Arc<dyn ExecBackend>,
    timeout_ms: u64,
}

impl ToolRunner {
    pub fn new(backend: Arc<dyn ExecBackend>, timeout_secs: u64) -> Self {
        Self { backend, timeout_ms: timeout_secs.max(1).saturating_mul(MILLIS_PER_SEC) }
    }

    /// Runs `argv` in `cwd` (the snapshot). `Err` is a ready-to-show note when the tool could not run or timed out.
    pub async fn run(&self, name: &str, argv: Vec<String>, cwd: &Path) -> Result<ExecResult, String> {
        let request = ExecRequest {
            argv,
            cwd: cwd.to_path_buf(),
            env: Default::default(),
            timeout_ms: self.timeout_ms,
            max_output_bytes: MAX_OUTPUT_BYTES,
        };
        let result = self.backend.execute(request).await;
        if let Some(failure) = &result.failure {
            return Err(format!("{name} could not run: {}", failure.message));
        }
        if result.timed_out {
            return Err(format!("{name} timed out after {}s", self.timeout_ms / MILLIS_PER_SEC));
        }
        Ok(result)
    }
}

/// Note for a run whose output held no usable report.
pub fn unreadable_note(name: &str, result: &ExecResult) -> String {
    let lines: Vec<&str> = result.output_tail.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(NOTE_LINES)..].join(" | ");
    let code = result.exit_code.map_or_else(|| "no exit code".to_string(), |c| format!("exit {c}"));
    format!("{name} produced no readable report ({code}): {tail}")
}

pub fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::application::FailureKind;
    use crate::sandbox::application::fakes::RecordingBackend;

    #[tokio::test]
    async fn passes_the_request_through_and_returns_the_result() {
        let backend = Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(0),
            output_tail: "ok".into(),
            ..Default::default()
        }));
        let runner = ToolRunner::new(backend.clone(), 7);
        let result = runner.run("t", vec!["tool".into(), "-x".into()], Path::new("/snap")).await.unwrap();
        assert_eq!(result.output_tail, "ok");
        let requests = backend.requests.lock().unwrap();
        assert_eq!(
            (requests[0].argv.clone(), requests[0].timeout_ms, requests[0].cwd.clone()),
            (vec!["tool".to_string(), "-x".to_string()], 7000, "/snap".into())
        );
    }

    #[tokio::test]
    async fn failures_and_timeouts_become_notes() {
        let failed = ToolRunner::new(
            Arc::new(RecordingBackend::answering(ExecResult::failed(FailureKind::Rejected, "nope"))),
            1,
        );
        assert_eq!(failed.run("t", vec!["x".into()], Path::new("/")).await.unwrap_err(), "t could not run: nope");
        let timed_out = ToolRunner::new(
            Arc::new(RecordingBackend::answering(ExecResult { timed_out: true, ..Default::default() })),
            2,
        );
        assert_eq!(timed_out.run("t", vec!["x".into()], Path::new("/")).await.unwrap_err(), "t timed out after 2s");
    }

    #[test]
    fn unreadable_note_quotes_the_last_lines() {
        let result = ExecResult { exit_code: Some(2), output_tail: "a\n\nb\nc\nd\n".into(), ..Default::default() };
        assert_eq!(unreadable_note("t", &result), "t produced no readable report (exit 2): b | c | d");
        assert!(unreadable_note("t", &ExecResult::default()).contains("no exit code"));
    }
}
