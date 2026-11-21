use std::path::PathBuf;

use futures::future::BoxFuture;

use super::runner::{ToolRunner, path_arg, unreadable_note};
use crate::quality::application::{CheckInput, CheckOutcome, QualityCheck};
use crate::quality::domain::parsers::gitleaks;
use crate::quality::domain::{Language, QualityTool};

const CONFIG_FILES: [&str; 2] = [".gitleaks.toml", ".gitleaksignore"];

pub struct GitleaksCheck {
    runner: ToolRunner,
    binary: Option<PathBuf>,
}

impl GitleaksCheck {
    pub fn new(runner: ToolRunner, binary: Option<PathBuf>) -> Self {
        Self { runner, binary }
    }

    fn argv(binary: &std::path::Path) -> Vec<String> {
        let args = [
            "dir",
            ".",
            "--redact",
            "--report-format",
            "json",
            "--report-path",
            "-",
            "--no-banner",
            "--log-level",
            "error",
            "--exit-code",
            "0",
        ];
        std::iter::once(path_arg(binary)).chain(args.map(String::from)).collect()
    }

    async fn check(&self, input: &CheckInput<'_>) -> CheckOutcome {
        if input.request.changed_files.is_empty() {
            return CheckOutcome::default();
        }
        let Some(binary) = &self.binary else {
            return CheckOutcome::skipped(
                "gitleaks is not installed: secret scan skipped (add `quality` to QGATE_TOOLCHAINS)",
            );
        };
        let result = match self.runner.run("gitleaks", Self::argv(binary), input.snapshot()).await {
            Ok(result) => result,
            Err(note) => return CheckOutcome::skipped(note),
        };
        let Some(findings) = gitleaks::parse(&result.output_tail, input.snapshot()) else {
            return CheckOutcome::skipped(unreadable_note("gitleaks", &result));
        };
        let mut outcome = CheckOutcome::found(findings);
        if input.changes.paths().any(|path| CONFIG_FILES.contains(&path.rsplit('/').next().unwrap_or(path))) {
            outcome
                .notes
                .push("this change edits the gitleaks config/ignore list: review it, it can hide secrets".into());
        }
        outcome
    }
}

impl QualityCheck for GitleaksCheck {
    fn tool(&self) -> QualityTool {
        QualityTool::Gitleaks
    }

    fn languages(&self) -> &'static [Language] {
        &[]
    }

    fn run<'a>(&'a self, input: &'a CheckInput<'a>) -> BoxFuture<'a, CheckOutcome> {
        Box::pin(self.check(input))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::quality::application::QualityRequest;
    use crate::quality::domain::{ChangeIndex, ChangedFile};
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    const REPORT: &str = include_str!("../fixtures/gitleaks.json");

    fn run(
        binary: Option<&str>,
        backend: Arc<RecordingBackend>,
        files: Vec<ChangedFile>,
    ) -> (CheckOutcome, Arc<RecordingBackend>) {
        let check = GitleaksCheck::new(ToolRunner::new(backend.clone(), 60), binary.map(PathBuf::from));
        let request =
            QualityRequest { snapshot: "/snap".into(), base_sha: None, head_sha: "h".into(), changed_files: files };
        let changes = ChangeIndex::new(&request.changed_files);
        let languages = Default::default();
        let input = CheckInput { request: &request, changes: &changes, languages: &languages };
        let outcome = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(check.check(&input));
        (outcome, backend)
    }

    fn backend(output: &str) -> Arc<RecordingBackend> {
        Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(0),
            output_tail: output.into(),
            ..Default::default()
        }))
    }

    fn touched(path: &str) -> ChangedFile {
        ChangedFile::new(path, true, Default::default())
    }

    #[test]
    fn scans_the_snapshot_and_parses_the_report() {
        let (outcome, backend) = run(Some("/tc/gitleaks"), backend(REPORT), vec![touched("a.go")]);
        assert_eq!(outcome.findings.len(), 1);
        assert!(outcome.notes.is_empty());
        let request = backend.requests.lock().unwrap()[0].clone();
        assert_eq!(request.cwd, Path::new("/snap"));
        assert_eq!(&request.argv[..3], ["/tc/gitleaks", "dir", "."]);
        assert!(request.argv.contains(&"--redact".to_string()));
    }

    #[test]
    fn not_installed_is_a_note_and_nothing_runs() {
        let (outcome, backend) = run(None, backend(REPORT), vec![touched("a.go")]);
        assert!(outcome.findings.is_empty());
        assert!(outcome.notes[0].starts_with("gitleaks is not installed"));
        assert!(backend.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn nothing_changed_means_nothing_to_scan() {
        let (outcome, backend) = run(Some("/tc/gitleaks"), backend(REPORT), vec![]);
        assert_eq!(outcome, CheckOutcome::default());
        assert!(backend.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn unreadable_output_is_a_note_and_config_edits_are_flagged() {
        let (outcome, _) = run(Some("/tc/gitleaks"), backend("boom"), vec![touched("a.go")]);
        assert!(outcome.notes[0].starts_with("gitleaks produced no readable report"));
        let (outcome, _) = run(Some("/tc/gitleaks"), backend("[]"), vec![touched("sub/.gitleaks.toml")]);
        assert!(outcome.notes[0].contains("gitleaks config"));
    }
}
