//! Quality bounded context: deterministic checks for what SonarQube does not cover, graded on the new code only
//! ("clean as you code"): dependency vulnerabilities (osv-scanner, govulncheck), leaked secrets (gitleaks) and
//! architecture rules (Go import boundaries through golangci-lint's depguard).
//!
//! Every tool runs through the sandbox's `ExecBackend` against a commit snapshot. A missing tool or an
//! unselected language skips the check with a note; it never fails the review. Java architecture rules
//! (ArchUnit) are test based: repositories add those tests themselves and qgate's test runner executes them.

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use application::{QualityConfig, QualityReport, QualityRequest, RunQualityChecks};
pub use domain::{
    ChangedFile, DependencyRef, ImportBoundary, Language, QualityCategory, QualityFinding, QualityRule,
    QualitySeverity, QualityTool, Verdict,
};
pub use infrastructure::{ToolchainPaths, default_checks};

use std::sync::Arc;

use crate::sandbox::application::ExecBackend;

/// Composition root: the use case wired to the real tool adapters running through `backend`.
pub fn build(backend: Arc<dyn ExecBackend>, config: QualityConfig) -> RunQualityChecks {
    RunQualityChecks::new(default_checks(backend, &config), config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    #[tokio::test]
    async fn build_wires_every_tool_and_skips_the_missing_ones_with_notes() {
        let backend = Arc::new(RecordingBackend::answering(ExecResult::default()));
        let config = QualityConfig::new([Language::Go], ToolchainPaths::default())
            .with_boundaries(vec![])
            .with_tools([QualityTool::Gitleaks, QualityTool::OsvScanner]);
        let request = QualityRequest {
            snapshot: "/snap".into(),
            base_sha: None,
            head_sha: "h".into(),
            changed_files: vec![ChangedFile::new("go.mod", true, Default::default())],
        };

        let report = build(backend.clone(), config).execute(&request).await;

        assert!(report.findings.is_empty());
        assert_eq!(report.notes.len(), 2, "{:?}", report.notes);
        assert!(report.notes.iter().all(|n| n.contains("not installed")));
        assert!(backend.requests.lock().unwrap().is_empty(), "nothing runs without a binary");
    }
}
