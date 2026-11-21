use std::sync::Arc;

use futures::future::BoxFuture;

use super::finding_mapper::regression_to_finding;
use crate::impact::application::{BaseRunStore, DEFAULT_MAX_PACKAGES, DetectRequest};
use crate::review::application::{RegressionChecks, RegressionRequest, ToolOutcome};
use crate::sandbox::application::ExecBackend;

pub struct RegressionTools {
    exec: Arc<dyn ExecBackend>,
    base_runs: Option<Arc<dyn BaseRunStore>>,
}

impl RegressionTools {
    pub fn new(exec: Arc<dyn ExecBackend>, base_runs: Option<Arc<dyn BaseRunStore>>) -> Self {
        Self { exec, base_runs }
    }
}

impl RegressionChecks for RegressionTools {
    fn run<'a>(&'a self, request: &'a RegressionRequest<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let detect_request = DetectRequest {
                base_sha: request.base_sha.to_string(),
                head_sha: request.head_sha.to_string(),
                base_snapshot: request.base_snapshot.to_path_buf(),
                head_snapshot: request.head_snapshot.to_path_buf(),
                changed_files: request.changes.files.iter().map(|f| f.path.clone()).collect(),
                max_packages: DEFAULT_MAX_PACKAGES,
            };
            let report = crate::impact::build(self.exec.clone(), self.base_runs.clone()).execute(&detect_request).await;
            let findings = report
                .regressions
                .iter()
                .map(|r| regression_to_finding(r, crate::impact::locate_failure(request.head_snapshot, r)))
                .collect();
            ToolOutcome { findings, notes: report.notes }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::ChangeSet;
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    #[tokio::test]
    async fn a_snapshot_that_is_not_a_go_module_becomes_a_note_not_a_finding() {
        let (head, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let changes = ChangeSet { files: vec![] };
        let request = RegressionRequest {
            base_sha: &"b".repeat(40),
            head_sha: &"h".repeat(40),
            base_snapshot: base.path(),
            head_snapshot: head.path(),
            changes: &changes,
        };
        let tools = RegressionTools::new(Arc::new(RecordingBackend::answering(ExecResult::default())), None);
        let outcome = tools.run(&request).await;
        assert!(outcome.findings.is_empty());
        assert!(outcome.notes.iter().any(|n| n.contains("not a Go module")), "{:?}", outcome.notes);
    }
}
