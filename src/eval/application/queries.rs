use std::sync::Arc;

use super::ports::{EvalError, EvalStore};
use crate::eval::domain::compare::{Comparison, DEFAULT_MAX_DROP, compare};
use crate::eval::domain::run::EvalRun;
use crate::history::application::RunProvenance;

pub const MAX_LISTED_RUNS: u32 = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct RunEntry {
    pub run: EvalRun,
    pub regression: bool,
}

pub struct ListEvalRuns {
    store: Arc<dyn EvalStore>,
}

impl ListEvalRuns {
    pub fn new(store: Arc<dyn EvalStore>) -> Self {
        Self { store }
    }

    pub fn execute(&self, limit: u32) -> Result<Vec<RunEntry>, EvalError> {
        let limit = limit.clamp(1, MAX_LISTED_RUNS) as usize;
        let mut runs = self.store.list(limit as u32 + 1)?;
        let entries = (0..runs.len().min(limit))
            .map(|index| RunEntry {
                regression: runs
                    .get(index + 1)
                    .is_some_and(|previous| compare(&runs[index], previous, DEFAULT_MAX_DROP).regressed),
                run: runs[index].clone(),
            })
            .collect();
        runs.clear();
        Ok(entries)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunDetail {
    pub run: EvalRun,
    pub previous_id: Option<i64>,
    pub comparison: Option<Comparison>,
}

pub struct GetEvalRun {
    store: Arc<dyn EvalStore>,
}

impl GetEvalRun {
    pub fn new(store: Arc<dyn EvalStore>) -> Self {
        Self { store }
    }

    pub fn execute(&self, id: i64) -> Result<Option<RunDetail>, EvalError> {
        let Some(run) = self.store.get(id)? else { return Ok(None) };
        let previous = self.store.previous_to(id)?;
        let comparison = previous.as_ref().map(|previous| compare(&run, previous, DEFAULT_MAX_DROP));
        Ok(Some(RunDetail { previous_id: previous.map(|p| p.id), comparison, run }))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusView {
    pub latest: Option<RunEntry>,
    pub regression: bool,
    pub current_provenance: RunProvenance,
    pub matches_current: bool,
}

pub struct EvalStatus {
    store: Arc<dyn EvalStore>,
}

impl EvalStatus {
    pub fn new(store: Arc<dyn EvalStore>) -> Self {
        Self { store }
    }

    pub fn execute(&self, current: RunProvenance) -> Result<StatusView, EvalError> {
        let latest = match self.store.latest()? {
            Some(run) => {
                let regression = self
                    .store
                    .previous_to(run.id)?
                    .is_some_and(|previous| compare(&run, &previous, DEFAULT_MAX_DROP).regressed);
                Some(RunEntry { run, regression })
            }
            None => None,
        };
        let matches_current = latest.as_ref().is_some_and(|entry| entry.run.provenance == current);
        let regression = latest.as_ref().is_some_and(|entry| entry.regression);
        Ok(StatusView { latest, regression, current_provenance: current, matches_current })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::application::test_support::{MemoryStore, provenance, sample_run};

    fn store_with(runs: &[&[bool]]) -> Arc<MemoryStore> {
        let store = Arc::new(MemoryStore::default());
        for caught in runs {
            store.save(&sample_run(caught)).unwrap();
        }
        store
    }

    #[test]
    fn list_flags_a_regression_against_the_previous_run() {
        let store = store_with(&[&[true, true], &[true, true], &[true, false]]);

        let entries = ListEvalRuns::new(store).execute(10).unwrap();

        let flags: Vec<(i64, bool)> = entries.iter().map(|e| (e.run.id, e.regression)).collect();
        assert_eq!(flags, [(3, true), (2, false), (1, false)]);
    }

    #[test]
    fn list_respects_the_limit_but_still_compares_the_last_entry() {
        let store = store_with(&[&[true, true], &[true, false]]);

        let entries = ListEvalRuns::new(store).execute(1).unwrap();

        assert_eq!(entries.len(), 1);
        assert!(entries[0].regression);
    }

    #[test]
    fn get_returns_the_run_with_a_comparison_to_its_predecessor() {
        let store = store_with(&[&[true, true], &[true, false]]);
        let use_case = GetEvalRun::new(store);

        let detail = use_case.execute(2).unwrap().unwrap();
        let first = use_case.execute(1).unwrap().unwrap();

        assert_eq!(detail.previous_id, Some(1));
        assert!(detail.comparison.unwrap().regressed);
        assert!(first.comparison.is_none() && first.previous_id.is_none());
        assert!(use_case.execute(9).unwrap().is_none());
    }

    #[test]
    fn status_without_any_eval_has_nothing_to_match() {
        let status = EvalStatus::new(store_with(&[])).execute(provenance()).unwrap();

        assert!(status.latest.is_none() && !status.regression && !status.matches_current);
    }

    #[test]
    fn status_reports_regression_and_whether_the_latest_matches_the_current_setup() {
        let store = store_with(&[&[true, true], &[true, false]]);
        let use_case = EvalStatus::new(store);

        let same = use_case.execute(provenance()).unwrap();
        let other = use_case.execute(RunProvenance { model: "new".into(), ..provenance() }).unwrap();

        assert!(same.regression && same.matches_current);
        assert!(!other.matches_current);
        assert_eq!(other.current_provenance.model, "new");
    }
}
