use std::path::Path;
use std::sync::Arc;

use super::supplementary_ports::{
    CommitChange, CommitChanges, QualityChecks, QualityRequest, RegressionChecks, RegressionRequest, SnapshotExporter,
    ToolOutcome,
};
use crate::review::domain::agent_results::AgentReport;
use crate::review::domain::gate::{GateDecision, GateInput, GatePolicy};
use crate::review::domain::report::ReviewReport;

pub struct SupplementarySettings {
    pub quality_enabled: bool,
    pub regression_enabled: bool,
}

pub struct RunSupplementaryChecks {
    commits: Arc<dyn CommitChanges>,
    snapshots: Arc<dyn SnapshotExporter>,
    quality: Arc<dyn QualityChecks>,
    regression: Arc<dyn RegressionChecks>,
    settings: SupplementarySettings,
}

impl RunSupplementaryChecks {
    pub fn new(
        commits: Arc<dyn CommitChanges>,
        snapshots: Arc<dyn SnapshotExporter>,
        quality: Arc<dyn QualityChecks>,
        regression: Arc<dyn RegressionChecks>,
        settings: SupplementarySettings,
    ) -> Self {
        Self { commits, snapshots, quality, regression, settings }
    }

    pub async fn execute(
        &self,
        head_snapshot: &Path,
        report: &mut ReviewReport,
        policy: GatePolicy,
        in_progress: bool,
    ) {
        let outcome = self.run(head_snapshot).await;
        let acceptance = GatePolicy { fail_on_unmet_criteria: policy.fail_on_unmet_criteria && !in_progress, ..policy };
        fold_into_report(report, outcome, &acceptance);
    }

    pub async fn run(&self, head_snapshot: &Path) -> ToolOutcome {
        let change = match self.commits.load().await {
            Ok(change) => change,
            Err(e) => {
                return ToolOutcome {
                    findings: Vec::new(),
                    notes: vec![format!("quality/regression checks skipped: {e}")],
                };
            }
        };
        let regression_on = self.settings.regression_enabled;
        let mut notes = Vec::new();
        let base_snapshot = match (&change.base_sha, regression_on) {
            (Some(base), true) => self
                .snapshots
                .export(base)
                .await
                .map_err(|e| notes.push(format!("regression check skipped: cannot export the base commit: {e:#}")))
                .ok(),
            _ => None,
        };
        let run_regression = regression_on && (base_snapshot.is_some() || notes.is_empty());
        let mut outcome = ToolOutcome::default();
        if self.settings.quality_enabled {
            let request = QualityRequest {
                snapshot: head_snapshot,
                base_sha: change.base_sha.as_deref(),
                head_sha: &change.head_sha,
                changes: &change.changes,
            };
            merge(&mut outcome, self.quality.run(&request).await);
        }
        if run_regression {
            let base = base_snapshot.as_ref().map(|s| s.path());
            merge(&mut outcome, self.regression_outcome(head_snapshot, base, &change).await);
        }
        notes.extend(outcome.notes);
        outcome.notes = notes;
        outcome
    }

    async fn regression_outcome(
        &self,
        head_snapshot: &Path,
        base_snapshot: Option<&Path>,
        change: &CommitChange,
    ) -> ToolOutcome {
        let skipped = |note: &str| ToolOutcome { findings: Vec::new(), notes: vec![note.to_string()] };
        let (Some(base_sha), Some(base_snapshot)) = (change.base_sha.as_deref(), base_snapshot) else {
            return skipped("regression check skipped: there is no base commit to compare with");
        };
        let request = RegressionRequest {
            base_sha,
            head_sha: &change.head_sha,
            base_snapshot,
            head_snapshot,
            changes: &change.changes,
        };
        self.regression.run(&request).await
    }
}

fn merge(into: &mut ToolOutcome, other: ToolOutcome) {
    into.findings.extend(other.findings);
    into.notes.extend(other.notes);
}

pub fn fold_into_report(report: &mut ReviewReport, outcome: ToolOutcome, policy: &GatePolicy) {
    if let Some(agents) = report.agents.as_mut() {
        agents.notes.extend(outcome.notes.iter().cloned());
    }
    for note in &outcome.notes {
        eprintln!("qgate: {note}");
    }
    report.findings.extend(outcome.findings);
    report.gate = decide(report, policy);
}

pub fn decide(report: &ReviewReport, policy: &GatePolicy) -> GateDecision {
    let agents: Option<&AgentReport> = report.agents.as_ref();
    policy.decide(GateInput {
        findings: &report.findings,
        requirements: report.requirements.as_ref(),
        checks: &report.checks,
        incomplete: report.gate.incomplete,
        agents,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use futures::future::BoxFuture;

    use super::*;
    use crate::review::application::ports::SourceError;
    use crate::review::application::supplementary_ports::ExportedSnapshot;
    use crate::review::domain::change_set::ChangeSet;
    use crate::review::domain::finding::{Category, Confidence, Finding, FindingSource, Severity, fixtures::finding};
    use crate::review::domain::report::ReviewReport;

    struct FakeCommits(Mutex<Option<Result<CommitChange, SourceError>>>);

    impl CommitChanges for FakeCommits {
        fn load(&self) -> BoxFuture<'_, Result<CommitChange, SourceError>> {
            let result = self.0.lock().unwrap().take().unwrap();
            Box::pin(async move { result })
        }
    }

    struct Snapshot(PathBuf);

    impl ExportedSnapshot for Snapshot {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    struct FakeSnapshots(bool);

    impl SnapshotExporter for FakeSnapshots {
        fn export<'a>(&'a self, _sha: &'a str) -> BoxFuture<'a, anyhow::Result<Box<dyn ExportedSnapshot>>> {
            let ok = self.0;
            Box::pin(async move {
                if ok { Ok(Box::new(Snapshot("/base".into())) as _) } else { Err(anyhow::anyhow!("boom")) }
            })
        }
    }

    struct Canned(Vec<String>, Vec<Finding>);

    impl Canned {
        fn outcome(&self) -> ToolOutcome {
            ToolOutcome { findings: self.1.clone(), notes: self.0.clone() }
        }
    }

    impl QualityChecks for Canned {
        fn run<'a>(&'a self, _: &'a QualityRequest<'a>) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move { self.outcome() })
        }
    }

    impl RegressionChecks for Canned {
        fn run<'a>(&'a self, _: &'a RegressionRequest<'a>) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move { self.outcome() })
        }
    }

    fn change(base: Option<&str>) -> CommitChange {
        CommitChange { head_sha: "h".into(), base_sha: base.map(String::from), changes: ChangeSet { files: vec![] } }
    }

    fn use_case(
        change: Result<CommitChange, SourceError>,
        export_ok: bool,
        (quality_enabled, regression_enabled): (bool, bool),
    ) -> RunSupplementaryChecks {
        RunSupplementaryChecks::new(
            Arc::new(FakeCommits(Mutex::new(Some(change)))),
            Arc::new(FakeSnapshots(export_ok)),
            Arc::new(Canned(vec!["q-note".into()], vec![])),
            Arc::new(Canned(vec!["r-note".into()], vec![])),
            SupplementarySettings { quality_enabled, regression_enabled },
        )
    }

    #[tokio::test]
    async fn both_checks_run_with_a_base_and_their_notes_are_combined() {
        let checks = use_case(Ok(change(Some("b"))), true, (true, true));
        let outcome = checks.run(Path::new("/head")).await;
        assert_eq!(outcome.notes, vec!["q-note", "r-note"]);
    }

    #[tokio::test]
    async fn regression_is_skipped_without_a_base_and_quality_can_be_disabled() {
        let outcome = use_case(Ok(change(None)), true, (true, true)).run(Path::new("/h")).await;
        assert!(outcome.notes.iter().any(|n| n.contains("no base commit")), "{:?}", outcome.notes);
        let regression_only = use_case(Ok(change(Some("b"))), true, (false, true));
        let outcome = regression_only.run(Path::new("/h")).await;
        assert_eq!(outcome.notes, vec!["r-note"], "regression runs for any language; suites self-detect");
        let off = use_case(Ok(change(None)), true, (false, false));
        assert_eq!(off.run(Path::new("/h")).await, ToolOutcome::default());
    }

    #[tokio::test]
    async fn an_unexportable_base_is_noted_and_skips_regression_only() {
        let checks = use_case(Ok(change(Some("b"))), false, (true, true));
        let outcome = checks.run(Path::new("/h")).await;
        assert_eq!(outcome.notes.len(), 2, "{:?}", outcome.notes);
        assert!(outcome.notes[0].contains("cannot export the base commit: boom"));
        assert_eq!(outcome.notes[1], "q-note");
    }

    #[tokio::test]
    async fn an_unloadable_change_skips_everything_with_a_note() {
        let checks = use_case(Err(SourceError::Git("x".into())), true, (true, true));
        let outcome = checks.run(Path::new("/h")).await;
        assert_eq!(outcome.notes, vec!["quality/regression checks skipped: git failed: x"]);
        assert!(outcome.findings.is_empty());
    }

    fn report_with(findings: Vec<Finding>) -> ReviewReport {
        let mut report = ReviewReport { findings, ..ReviewReport::default() };
        report.gate = decide(&report, &GatePolicy::default());
        report
    }

    #[test]
    fn folding_nothing_leaves_the_gate_unchanged() {
        let policy = GatePolicy::default();
        for findings in
            [vec![], vec![finding("a.go", 1, Severity::Critical, "x")], vec![finding("a.go", 1, Severity::Minor, "x")]]
        {
            let mut report = report_with(findings);
            report.gate.incomplete = true;
            report.gate = decide(&report, &policy);
            let before = report.gate.clone();
            fold_into_report(&mut report, ToolOutcome::default(), &policy);
            assert_eq!(report.gate, before);
        }
    }

    #[test]
    fn folded_blocking_findings_fail_the_gate_and_warnings_do_not() {
        let policy = GatePolicy::default();
        let mut report = report_with(vec![]);
        assert!(report.gate.passed);
        let warning = Finding { severity: Severity::Minor, ..finding("go.mod", 1, Severity::Minor, "e") };
        let blocker = Finding {
            category: Category::Architecture,
            confidence: Confidence::High,
            source: FindingSource::StaticTool("depguard".into()),
            ..finding("a.go", 2, Severity::Critical, "e")
        };
        let notes = vec!["osv-scanner not installed".to_string()];
        fold_into_report(&mut report, ToolOutcome { findings: vec![warning, blocker], notes }, &policy);
        assert!(!report.gate.passed);
        assert_eq!(report.gate.blocking, vec![1]);
        assert_eq!(report.findings.len(), 2);
    }

    #[test]
    fn notes_are_added_to_the_agent_report_when_there_is_one() {
        let mut report = report_with(vec![]);
        report.agents = Some(AgentReport::default());
        let outcome = ToolOutcome { findings: vec![], notes: vec!["n1".into()] };
        fold_into_report(&mut report, outcome, &GatePolicy::default());
        assert_eq!(report.agents.unwrap().notes, vec!["n1"]);
    }
}
