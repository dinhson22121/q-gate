use std::path::PathBuf;

use futures::future::BoxFuture;

use super::runner::{ToolRunner, path_arg, unreadable_note};
use crate::quality::application::{CheckInput, CheckOutcome, QualityCheck};
use crate::quality::domain::parsers::govulncheck;
use crate::quality::domain::{Language, QualityTool};

const GO_MOD: &str = "go.mod";

/// Go vulnerabilities whose vulnerable code is actually reachable (call-graph analysis).
pub struct GovulncheckCheck {
    runner: ToolRunner,
    binary: Option<PathBuf>,
}

impl GovulncheckCheck {
    pub fn new(runner: ToolRunner, binary: Option<PathBuf>) -> Self {
        Self { runner, binary }
    }

    async fn check(&self, input: &CheckInput<'_>) -> CheckOutcome {
        let touches_go =
            input.changes.paths().any(|p| p.ends_with(".go") || p.ends_with("go.mod") || p.ends_with("go.sum"));
        if !touches_go || !input.snapshot().join(GO_MOD).is_file() {
            return CheckOutcome::default();
        }
        let Some(binary) = &self.binary else {
            return CheckOutcome::skipped(
                "govulncheck is not installed: Go reachability scan skipped (add `go` to QGATE_TOOLCHAINS)",
            );
        };
        let argv = vec![path_arg(binary), "-json".into(), "./...".into()];
        let result = match self.runner.run("govulncheck", argv, input.snapshot()).await {
            Ok(result) => result,
            Err(note) => return CheckOutcome::skipped(note),
        };
        match govulncheck::parse(&result.output_tail, input.snapshot()) {
            Some(findings) => CheckOutcome::found(findings),
            None => CheckOutcome::skipped(unreadable_note("govulncheck", &result)),
        }
    }
}

impl QualityCheck for GovulncheckCheck {
    fn tool(&self) -> QualityTool {
        QualityTool::Govulncheck
    }

    fn languages(&self) -> &'static [Language] {
        &[Language::Go]
    }

    fn run<'a>(&'a self, input: &'a CheckInput<'a>) -> BoxFuture<'a, CheckOutcome> {
        Box::pin(self.check(input))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::quality::application::QualityRequest;
    use crate::quality::domain::{ChangeIndex, ChangedFile};
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    const STREAM: &str = include_str!("../fixtures/govulncheck.json");

    fn run(binary: Option<&str>, with_go_mod: bool, output: &str, changed: &[&str]) -> (CheckOutcome, usize) {
        let snapshot = tempfile::tempdir().unwrap();
        if with_go_mod {
            std::fs::write(snapshot.path().join("go.mod"), "module m\n").unwrap();
        }
        let backend = Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(0),
            output_tail: output.into(),
            ..Default::default()
        }));
        let check = GovulncheckCheck::new(ToolRunner::new(backend.clone(), 60), binary.map(PathBuf::from));
        let files = changed.iter().map(|p| ChangedFile::new(*p, true, Default::default())).collect();
        let request = QualityRequest {
            snapshot: snapshot.path().into(),
            base_sha: None,
            head_sha: "h".into(),
            changed_files: files,
        };
        let changes = ChangeIndex::new(&request.changed_files);
        let languages = Default::default();
        let input = CheckInput { request: &request, changes: &changes, languages: &languages };
        let outcome = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(check.check(&input));
        let runs = backend.requests.lock().unwrap().len();
        (outcome, runs)
    }

    #[test]
    fn runs_for_go_changes_in_a_module() {
        let (outcome, runs) = run(Some("/tc/govulncheck"), true, STREAM, &["main.go"]);
        assert_eq!((outcome.findings.len(), runs), (1, 1));
    }

    #[test]
    fn skips_quietly_without_go_changes_or_module() {
        assert_eq!(run(Some("/tc/govulncheck"), true, STREAM, &["README.md"]), (CheckOutcome::default(), 0));
        assert_eq!(run(Some("/tc/govulncheck"), false, STREAM, &["main.go"]), (CheckOutcome::default(), 0));
    }

    #[test]
    fn missing_binary_and_unreadable_output_are_notes() {
        let (outcome, runs) = run(None, true, "", &["main.go"]);
        assert!(outcome.notes[0].starts_with("govulncheck is not installed"));
        assert_eq!(runs, 0);
        let (outcome, _) = run(Some("/tc/govulncheck"), true, "govulncheck: loading packages: boom", &["main.go"]);
        assert!(outcome.notes[0].contains("no readable report"));
    }
}
