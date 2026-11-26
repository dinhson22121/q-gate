use std::sync::Arc;

use futures::future::BoxFuture;

use super::finding_mapper::regression_to_finding;
use crate::impact::application::{BaseRunStore, DEFAULT_MAX_PACKAGES, DetectRequest, RegressionReport};
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
            let go = crate::impact::build(self.exec.clone(), self.base_runs.clone());
            let suites = crate::impact::build_suites(self.exec.clone(), self.base_runs.clone());
            let (go_report, suite_report) =
                futures::join!(go.execute(&detect_request), suites.execute(&detect_request));
            let report = combine(go_report, suite_report);
            let findings = report
                .regressions
                .iter()
                .map(|r| regression_to_finding(r, crate::impact::locate_failure(request.head_snapshot, r)))
                .collect();
            ToolOutcome { findings, notes: report.notes }
        })
    }
}

fn combine(go: RegressionReport, suites: Option<RegressionReport>) -> RegressionReport {
    let go_skipped = go.skipped_reason.is_some();
    match suites {
        None if go_skipped => RegressionReport::skipped(
            "no supported test suite found (Go modules, Maven, Gradle, pytest, jest, vitest or Cargo)",
        ),
        None => go,
        Some(suites) if go_skipped => suites,
        Some(suites) => {
            let mut report = go;
            report.merge(suites);
            report
        }
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
        assert!(outcome.notes.iter().any(|n| n.contains("no supported test suite found")), "{:?}", outcome.notes);
    }

    #[test]
    fn go_and_suite_reports_combine_without_hiding_either() {
        use crate::impact::application::Regression;
        let go_skip = || RegressionReport::skipped("not a Go module");
        let mut maven = RegressionReport { packages_tested: vec!["maven".into()], ..RegressionReport::default() };
        maven.regressions.push(Regression::new("C", "t", String::new()));
        assert_eq!(combine(go_skip(), Some(maven.clone())), maven, "a skipped Go check adds no noise");
        assert!(combine(go_skip(), None).skipped_reason.unwrap().contains("no supported test suite"));
        let go = RegressionReport { packages_tested: vec!["m/api".into()], ..RegressionReport::default() };
        assert_eq!(combine(go.clone(), None), go);
        let both = combine(go, Some(maven));
        assert_eq!(both.packages_tested, vec!["m/api", "maven"]);
        assert_eq!(both.regressions.len(), 1);
    }
}
