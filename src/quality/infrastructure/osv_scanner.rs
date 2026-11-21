use std::path::PathBuf;

use futures::future::BoxFuture;

use super::runner::{ToolRunner, path_arg, unreadable_note};
use crate::quality::application::{CheckInput, CheckOutcome, QualityCheck};
use crate::quality::domain::parsers::osv;
use crate::quality::domain::{Language, QualityTool};

/// Dependency manifests and lockfiles of Go and Java (Maven, Gradle) projects. Only a change to one of them can
/// introduce a vulnerable dependency, so the scan is skipped otherwise.
const MANIFESTS: [&str; 8] = [
    "go.mod",
    "go.sum",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "gradle.lockfile",
    "buildscript-gradle.lockfile",
    "libs.versions.toml",
];
/// osv-scanner exit codes for "no supported packages / lockfiles found".
const NO_PACKAGES: [i32; 2] = [127, 128];

/// Known-vulnerable dependencies of the snapshot (OSV database; needs network access to api.osv.dev).
pub struct OsvScannerCheck {
    runner: ToolRunner,
    binary: Option<PathBuf>,
}

impl OsvScannerCheck {
    pub fn new(runner: ToolRunner, binary: Option<PathBuf>) -> Self {
        Self { runner, binary }
    }

    async fn check(&self, input: &CheckInput<'_>) -> CheckOutcome {
        if !input.changes.paths().any(|path| MANIFESTS.contains(&path.rsplit('/').next().unwrap_or(path))) {
            return CheckOutcome::default();
        }
        let Some(binary) = &self.binary else {
            return CheckOutcome::skipped(
                "osv-scanner is not installed: dependency scan skipped (add `quality` to QGATE_TOOLCHAINS)",
            );
        };
        let argv = [path_arg(binary)]
            .into_iter()
            .chain(["scan", "source", "--format", "json", "-r", "."].map(String::from))
            .collect();
        let result = match self.runner.run("osv-scanner", argv, input.snapshot()).await {
            Ok(result) => result,
            Err(note) => return CheckOutcome::skipped(note),
        };
        match osv::parse(&result.output_tail, input.snapshot()) {
            Some(findings) => CheckOutcome::found(findings),
            None if result.exit_code.is_some_and(|code| NO_PACKAGES.contains(&code)) => CheckOutcome::default(),
            None => CheckOutcome::skipped(unreadable_note("osv-scanner", &result)),
        }
    }
}

impl QualityCheck for OsvScannerCheck {
    fn tool(&self) -> QualityTool {
        QualityTool::OsvScanner
    }

    fn languages(&self) -> &'static [Language] {
        &[Language::Go, Language::Java]
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

    const REPORT: &str = include_str!("../fixtures/osv.json");

    fn run(binary: Option<&str>, exit_code: i32, output: &str, changed: &[&str]) -> (CheckOutcome, usize) {
        let backend = Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(exit_code),
            output_tail: output.into(),
            ..Default::default()
        }));
        let check = OsvScannerCheck::new(ToolRunner::new(backend.clone(), 60), binary.map(PathBuf::from));
        let files = changed.iter().map(|p| ChangedFile::new(*p, true, Default::default())).collect();
        let request =
            QualityRequest { snapshot: "/snap".into(), base_sha: None, head_sha: "h".into(), changed_files: files };
        let changes = ChangeIndex::new(&request.changed_files);
        let languages = Default::default();
        let input = CheckInput { request: &request, changes: &changes, languages: &languages };
        let outcome = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(check.check(&input));
        let runs = backend.requests.lock().unwrap().len();
        (outcome, runs)
    }

    #[test]
    fn scans_when_a_manifest_changed() {
        let (outcome, runs) = run(Some("/tc/osv-scanner"), 1, REPORT, &["svc/go.mod"]);
        assert_eq!((outcome.findings.len(), runs), (3, 1));
    }

    #[test]
    fn skips_silently_when_no_manifest_changed() {
        let (outcome, runs) = run(Some("/tc/osv-scanner"), 1, REPORT, &["main.go"]);
        assert_eq!((outcome, runs), (CheckOutcome::default(), 0));
    }

    #[test]
    fn missing_binary_is_a_note() {
        let (outcome, runs) = run(None, 0, "", &["pom.xml"]);
        assert!(outcome.notes[0].starts_with("osv-scanner is not installed"));
        assert_eq!(runs, 0);
    }

    #[test]
    fn no_packages_is_quiet_but_other_failures_are_noted() {
        let (outcome, _) = run(Some("/tc/osv-scanner"), 128, "No package sources found", &["go.mod"]);
        assert_eq!(outcome, CheckOutcome::default());
        let (outcome, _) = run(Some("/tc/osv-scanner"), 2, "network down", &["go.mod"]);
        assert!(outcome.notes[0].contains("exit 2"));
    }
}
