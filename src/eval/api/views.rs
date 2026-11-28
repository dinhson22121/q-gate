use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::eval::application::{ModelCoverage, RunDetail, RunEntry, StatusView};
use crate::eval::domain::compare::Comparison;
use crate::eval::domain::run::{AggregateMetrics, EvalRun};
use crate::eval::domain::scoring::CaseScore;
use crate::eval::domain::stats::{Counts, CriterionCounts};
use crate::history::application::RunProvenance;

pub const STALE_NOTE: &str = "no eval for the current model/prompt";
pub const NO_EVAL_NOTE: &str = "no eval has been run yet";

#[derive(Debug, Serialize)]
pub struct Summary {
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub f1: Option<f64>,
    pub f1_stddev: Option<f64>,
    pub ac_accuracy: Option<f64>,
    pub unverifiable_rate: Option<f64>,
    pub counts: Counts,
    pub criteria: CriterionCounts,
}

impl From<&AggregateMetrics> for Summary {
    fn from(metrics: &AggregateMetrics) -> Self {
        Self {
            precision: metrics.precision.mean,
            recall: metrics.recall.mean,
            f1: metrics.f1.mean,
            f1_stddev: metrics.f1.stddev,
            ac_accuracy: metrics.ac_accuracy.mean,
            unverifiable_rate: metrics.unverifiable_rate.mean,
            counts: metrics.counts,
            criteria: metrics.criteria,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RunItem {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub provenance: RunProvenance,
    pub runs_per_case: u32,
    pub case_count: u32,
    pub with_tests: bool,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub regression: bool,
    pub summary: Summary,
}

impl RunItem {
    fn of(run: &EvalRun, regression: bool) -> Self {
        Self {
            id: run.id,
            created_at: run.created_at,
            provenance: run.provenance.clone(),
            runs_per_case: run.runs_per_case,
            case_count: run.metrics.case_count,
            with_tests: run.with_tests,
            cost_usd: run.metrics.cost_usd,
            duration_ms: run.metrics.duration_ms,
            regression,
            summary: Summary::from(&run.metrics),
        }
    }
}

impl From<&RunEntry> for RunItem {
    fn from(entry: &RunEntry) -> Self {
        Self::of(&entry.run, entry.regression)
    }
}

#[derive(Debug, Serialize)]
pub struct RunDetailView {
    #[serde(flatten)]
    pub item: RunItem,
    pub metrics: AggregateMetrics,
    pub results: Vec<CaseScore>,
    pub previous_id: Option<i64>,
    pub comparison: Option<Comparison>,
}

impl From<RunDetail> for RunDetailView {
    fn from(detail: RunDetail) -> Self {
        let regression = detail.comparison.as_ref().is_some_and(|c| c.regressed);
        Self {
            item: RunItem::of(&detail.run, regression),
            metrics: detail.run.metrics,
            results: detail.run.results,
            previous_id: detail.previous_id,
            comparison: detail.comparison,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatusBody {
    pub latest: Option<RunItem>,
    pub regression: bool,
    pub current_provenance: RunProvenance,
    pub matches_current: bool,
    pub models: Vec<ModelItem>,
    pub note: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct ModelItem {
    pub model: String,
    pub repos: Vec<String>,
    pub has_eval: bool,
    pub latest_eval_id: Option<i64>,
}

impl From<ModelCoverage> for ModelItem {
    fn from(coverage: ModelCoverage) -> Self {
        Self {
            model: coverage.model,
            repos: coverage.repos,
            has_eval: coverage.has_eval,
            latest_eval_id: coverage.latest_eval_id,
        }
    }
}

impl From<StatusView> for StatusBody {
    fn from(status: StatusView) -> Self {
        let note = match (&status.latest, status.matches_current) {
            (None, _) => Some(NO_EVAL_NOTE),
            (Some(_), false) => Some(STALE_NOTE),
            (Some(_), true) => None,
        };
        Self {
            latest: status.latest.as_ref().map(RunItem::from),
            regression: status.regression,
            current_provenance: status.current_provenance,
            matches_current: status.matches_current,
            models: status.models.into_iter().map(ModelItem::from).collect(),
            note,
        }
    }
}
