use std::sync::Arc;

use super::ports::{EvalError, EvalStore, RepoModel};
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
        let runs = self.store.list(limit as u32 + 1)?;
        (0..runs.len().min(limit))
            .map(|index| {
                let run = &runs[index];
                let previous = match runs[index + 1..].iter().find(|candidate| !candidate.partial) {
                    Some(found) => Some(found.clone()),
                    None => self.store.previous_full_to(runs[runs.len() - 1].id)?,
                };
                let regression = previous.is_some_and(|previous| compare(run, &previous, DEFAULT_MAX_DROP).regressed);
                Ok(RunEntry { run: run.clone(), regression })
            })
            .collect()
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
        let previous = self.store.previous_full_to(id)?;
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
    pub models: Vec<ModelCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCoverage {
    pub model: String,
    pub repos: Vec<String>,
    pub has_eval: bool,
    pub latest_eval_id: Option<i64>,
}

pub struct EvalStatus {
    store: Arc<dyn EvalStore>,
}

impl EvalStatus {
    pub fn new(store: Arc<dyn EvalStore>) -> Self {
        Self { store }
    }

    pub fn execute(&self, current: RunProvenance, repo_models: &[RepoModel]) -> Result<StatusView, EvalError> {
        let latest = match self.store.latest_full()? {
            Some(run) => {
                let regression = self
                    .store
                    .previous_full_to(run.id)?
                    .is_some_and(|previous| compare(&run, &previous, DEFAULT_MAX_DROP).regressed);
                Some(RunEntry { run, regression })
            }
            None => None,
        };
        let matches_current = latest.as_ref().is_some_and(|entry| entry.run.provenance == current);
        let regression = latest.as_ref().is_some_and(|entry| entry.regression);
        let models = self.coverage(&current, repo_models)?;
        Ok(StatusView { latest, regression, current_provenance: current, matches_current, models })
    }

    fn coverage(&self, current: &RunProvenance, repo_models: &[RepoModel]) -> Result<Vec<ModelCoverage>, EvalError> {
        let mut models = vec![ModelCoverage {
            model: current.model.clone(),
            repos: Vec::new(),
            has_eval: false,
            latest_eval_id: None,
        }];
        for entry in repo_models {
            match models.iter_mut().find(|m| m.model == entry.model) {
                Some(existing) => existing.repos.push(entry.repo.clone()),
                None => models.push(ModelCoverage {
                    model: entry.model.clone(),
                    repos: vec![entry.repo.clone()],
                    has_eval: false,
                    latest_eval_id: None,
                }),
            }
        }
        for coverage in &mut models {
            let provenance = RunProvenance { model: coverage.model.clone(), ..current.clone() };
            let latest = self.store.latest_for(&provenance)?;
            coverage.has_eval = latest.is_some();
            coverage.latest_eval_id = latest.map(|run| run.id);
        }
        Ok(models)
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
    fn list_compares_against_the_previous_full_run_and_not_a_partial_one() {
        let store = store_with(&[&[true, true]]);
        store.save(&sample_run(&[false]).with_partial(true)).unwrap();
        store.save(&sample_run(&[true, false])).unwrap();
        store.save(&sample_run(&[true, true]).with_partial(true)).unwrap();

        let entries = ListEvalRuns::new(store).execute(10).unwrap();

        let flags: Vec<(i64, bool)> = entries.iter().map(|e| (e.run.id, e.regression)).collect();
        assert_eq!(flags, [(4, false), (3, true), (2, true), (1, false)]);
    }

    #[test]
    fn list_looks_past_the_page_for_the_previous_full_run() {
        let store = store_with(&[&[true, true]]);
        store.save(&sample_run(&[false]).with_partial(true)).unwrap();
        store.save(&sample_run(&[true, false])).unwrap();

        let entries = ListEvalRuns::new(store).execute(1).unwrap();

        assert_eq!(entries.len(), 1);
        assert!(entries[0].regression);
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
    fn get_compares_to_the_previous_full_run_skipping_partial_ones() {
        let store = store_with(&[&[true, true]]);
        store.save(&sample_run(&[false]).with_partial(true)).unwrap();
        store.save(&sample_run(&[true, true])).unwrap();
        let use_case = GetEvalRun::new(store);

        let full = use_case.execute(3).unwrap().unwrap();
        let partial = use_case.execute(2).unwrap().unwrap();

        assert_eq!(full.previous_id, Some(1));
        assert_eq!(partial.previous_id, Some(1));
        assert!(partial.comparison.unwrap().regressed);
    }

    #[test]
    fn status_reports_the_latest_full_run_ignoring_newer_partial_runs() {
        let store = store_with(&[&[true, true], &[true, true]]);
        store.save(&sample_run(&[false]).with_partial(true)).unwrap();

        let status = EvalStatus::new(store).execute(provenance(), &[]).unwrap();

        assert_eq!(status.latest.unwrap().run.id, 2);
        assert!(!status.regression);
    }

    #[test]
    fn status_without_any_eval_has_nothing_to_match() {
        let status = EvalStatus::new(store_with(&[])).execute(provenance(), &[]).unwrap();

        assert!(status.latest.is_none() && !status.regression && !status.matches_current);
    }

    #[test]
    fn status_reports_regression_and_whether_the_latest_matches_the_current_setup() {
        let store = store_with(&[&[true, true], &[true, false]]);
        let use_case = EvalStatus::new(store);

        let same = use_case.execute(provenance(), &[]).unwrap();
        let other = use_case.execute(RunProvenance { model: "new".into(), ..provenance() }, &[]).unwrap();

        assert!(same.regression && same.matches_current);
        assert!(!other.matches_current);
        assert_eq!(other.current_provenance.model, "new");
    }

    fn repo_model(repo: &str, model: &str) -> RepoModel {
        RepoModel { repo: repo.into(), model: model.into() }
    }

    #[test]
    fn status_covers_the_default_model_and_every_repo_override() {
        let store = store_with(&[&[true]]);
        let other = RunProvenance { model: "opus".into(), ..provenance() };
        store.save(&EvalRun::new(chrono::Utc::now(), other, 1, false, Vec::new())).unwrap();
        let repos = [repo_model("a", "m"), repo_model("b", "opus"), repo_model("c", "haiku"), repo_model("d", "opus")];

        let status = EvalStatus::new(store).execute(provenance(), &repos).unwrap();

        let summary: Vec<(&str, Vec<&str>, bool, Option<i64>)> = status
            .models
            .iter()
            .map(|m| (m.model.as_str(), m.repos.iter().map(String::as_str).collect(), m.has_eval, m.latest_eval_id))
            .collect();
        assert_eq!(
            summary,
            [
                ("m", vec!["a"], true, Some(1)),
                ("opus", vec!["b", "d"], true, Some(2)),
                ("haiku", vec!["c"], false, None)
            ]
        );
    }

    #[test]
    fn a_model_with_a_stale_prompt_hash_has_no_eval() {
        let store = store_with(&[&[true]]);

        let status =
            EvalStatus::new(store).execute(RunProvenance { prompt_hash: "new".into(), ..provenance() }, &[]).unwrap();

        assert!(!status.models[0].has_eval);
    }

    #[test]
    fn status_finds_an_older_eval_for_a_model_that_is_not_the_latest() {
        let store = store_with(&[&[true]]);
        let other = RunProvenance { model: "opus".into(), ..provenance() };
        store.save(&EvalRun::new(chrono::Utc::now(), other, 1, false, Vec::new())).unwrap();

        let status = EvalStatus::new(store).execute(provenance(), &[]).unwrap();

        assert!(!status.matches_current);
        assert_eq!(status.models[0].latest_eval_id, Some(1));
    }
}
