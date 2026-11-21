use std::collections::BTreeSet;
use std::sync::Arc;

use crate::history::application::{GetRun, HistoryError, MAX_PAGE_SIZE, RecentRuns, RunRepository, StoredRun};
use crate::policy::application::{FindingContext, PolicyError, RunHistory};
use crate::policy::domain::RunFacts;
use crate::review::application::CriterionStatus;

pub struct HistoryUseCases {
    repository: Arc<dyn RunRepository>,
}

impl HistoryUseCases {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }
}

fn policy_error(err: HistoryError) -> PolicyError {
    PolicyError::Storage(err.to_string())
}

fn facts(run: &StoredRun) -> Option<RunFacts> {
    let report = run.report.as_ref()?;
    let gate = &report.gate;
    let unmet_from_assessments = report
        .requirements
        .as_ref()
        .map_or(0, |r| r.assessments.iter().filter(|a| a.status == CriterionStatus::NotMet).count());
    Some(RunFacts {
        run_id: run.id,
        repo_path: run.repo_path.clone(),
        repo_name: run.repo_name.clone(),
        created_at: run.created_at,
        recorded_passed: gate.passed,
        findings: report.findings.iter().map(|f| (f.severity, f.category, f.confidence)).collect(),
        unmet_criteria: gate.unmet_criteria.len().max(unmet_from_assessments),
        failed_checks: gate.failed_checks.len(),
        incomplete: gate.incomplete,
        failed_tests: gate.failed_tests.len(),
        rule_violations: gate.rule_violations.len(),
        design_mismatches: report
            .agents
            .as_ref()
            .map(|a| a.design.iter().map(|d| d.kind).collect())
            .unwrap_or_default(),
    })
}

impl RunHistory for HistoryUseCases {
    fn recent(&self, limit: u32) -> Result<(Vec<RunFacts>, usize), PolicyError> {
        let runs = RecentRuns::new(self.repository.clone()).execute(limit).map_err(policy_error)?;
        let facts: Vec<RunFacts> = runs.iter().filter_map(facts).collect();
        let skipped = runs.len() - facts.len();
        Ok((facts, skipped))
    }

    fn finding(&self, run_id: i64, index: usize) -> Result<Option<FindingContext>, PolicyError> {
        let run = GetRun::new(self.repository.clone()).execute(run_id).map_err(policy_error)?;
        Ok(run.and_then(|run| {
            let finding = run.report.as_ref()?.findings.get(index)?.clone();
            Some(FindingContext {
                repo_path: run.repo_path.clone(),
                file: finding.file,
                line: finding.line,
                severity: finding.severity.as_str().into(),
                category: finding.category.as_str().into(),
                title: finding.title,
                explanation: finding.explanation,
                evidence: finding.evidence,
            })
        }))
    }

    fn repo_paths(&self) -> Result<Vec<String>, PolicyError> {
        let runs = RecentRuns::new(self.repository.clone()).execute(MAX_PAGE_SIZE).map_err(policy_error)?;
        Ok(runs.into_iter().map(|r| r.repo_path).collect::<BTreeSet<_>>().into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::application::{ReviewRun, RunStatus};
    use crate::history::domain::fixtures::{at, live_run};
    use crate::history::infrastructure::SqliteRunRepository;

    #[test]
    fn maps_stored_runs_to_gate_facts_and_findings() {
        let dir = tempfile::tempdir().unwrap();
        let repository = Arc::new(SqliteRunRepository::open(dir.path().join("qgate.db")).unwrap());
        let first = repository.insert(&live_run("app", at(20, 9))).unwrap();
        let errored = ReviewRun {
            status: RunStatus::Error,
            error: Some("boom".into()),
            report: None,
            ..live_run("other", at(20, 10))
        };
        repository.insert(&errored).unwrap();
        let history = HistoryUseCases::new(repository);

        let (runs, skipped) = history.recent(10).unwrap();
        assert_eq!((runs.len(), skipped), (1, 1));
        let run = &runs[0];
        assert_eq!((run.run_id, run.repo_path.as_str()), (first, "/src/app"));
        assert!(!run.findings.is_empty());
        assert!(!run.recorded_passed);

        let finding = history.finding(first, 0).unwrap().unwrap();
        assert_eq!(finding.repo_path, "/src/app");
        assert!(!finding.title.is_empty() && !finding.severity.is_empty());
        assert_eq!(history.finding(first, 99).unwrap(), None);
        assert_eq!(history.finding(999, 0).unwrap(), None);
        assert_eq!(history.repo_paths().unwrap(), vec!["/src/app".to_string(), "/src/other".to_string()]);
    }
}
