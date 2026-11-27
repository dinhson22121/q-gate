use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::case::ExpectedStatus;
use super::scoring::{CaseScore, CriterionVerdict};
use super::stats::{Counts, CriterionCounts, Stat};
use crate::history::application::RunProvenance;
use crate::review::application::Category;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalRun {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub provenance: RunProvenance,
    pub runs_per_case: u32,
    pub with_tests: bool,
    pub metrics: AggregateMetrics,
    pub results: Vec<CaseScore>,
}

impl EvalRun {
    pub fn new(
        created_at: DateTime<Utc>,
        provenance: RunProvenance,
        runs_per_case: u32,
        with_tests: bool,
        results: Vec<CaseScore>,
    ) -> Self {
        let metrics = aggregate(&results, runs_per_case);
        Self { id: 0, created_at, provenance, runs_per_case, with_tests, metrics, results }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub runs_per_case: u32,
    pub case_count: u32,
    pub counts: Counts,
    pub precision: Stat,
    pub recall: Stat,
    pub f1: Stat,
    pub ac_accuracy: Stat,
    pub unverifiable_rate: Stat,
    pub criteria: CriterionCounts,
    pub by_category: BTreeMap<String, CategoryMetrics>,
    pub cases: Vec<CaseSummary>,
    pub cost_usd: f64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub f1: Option<f64>,
    pub ac_accuracy: Option<f64>,
}

impl AggregateMetrics {
    pub fn headline(&self) -> Headline {
        Headline {
            precision: self.precision.mean,
            recall: self.recall.mean,
            f1: self.f1.mean,
            ac_accuracy: self.ac_accuracy.mean,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CategoryMetrics {
    pub counts: Counts,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub f1: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseSummary {
    pub case_id: String,
    pub runs: u32,
    pub counts: Counts,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub f1: Option<f64>,
    pub ac_accuracy: Option<f64>,
    pub criteria: CriterionCounts,
    pub expected: Vec<ExpectedSummary>,
    pub criteria_detail: Vec<CriterionSummary>,
    pub cost_usd: f64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedSummary {
    pub label: String,
    pub category: Category,
    pub caught_runs: u32,
    pub caught: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionSummary {
    pub index: usize,
    pub text: String,
    pub expect: ExpectedStatus,
    pub correct_runs: u32,
    pub correct: bool,
}

fn majority(hits: u32, runs: u32) -> bool {
    runs > 0 && hits * 2 >= runs
}

pub fn aggregate(results: &[CaseScore], runs_per_case: u32) -> AggregateMetrics {
    let cases = summarize_cases(results);
    let mut counts = Counts::default();
    let mut criteria = CriterionCounts::default();
    let mut by_repetition: BTreeMap<u32, (Counts, CriterionCounts)> = BTreeMap::new();
    for score in results {
        counts += score.counts();
        criteria += score.criterion_counts();
        let entry = by_repetition.entry(score.repetition).or_default();
        entry.0 += score.counts();
        entry.1 += score.criterion_counts();
    }
    let per_run = |metric: fn(&Counts, &CriterionCounts) -> Option<f64>| {
        Stat::of(by_repetition.values().map(|(c, ac)| metric(c, ac)))
    };
    AggregateMetrics {
        runs_per_case,
        case_count: cases.len() as u32,
        counts,
        precision: per_run(|c, _| c.precision()),
        recall: per_run(|c, _| c.recall()),
        f1: per_run(|c, _| c.f1()),
        ac_accuracy: per_run(|_, ac| ac.accuracy()),
        unverifiable_rate: per_run(|_, ac| ac.unverifiable_rate()),
        criteria,
        by_category: by_category(results),
        cases,
        cost_usd: results.iter().map(|r| r.cost_usd).sum(),
        duration_ms: results.iter().map(|r| r.duration_ms).sum(),
    }
}

fn by_category(results: &[CaseScore]) -> BTreeMap<String, CategoryMetrics> {
    let mut totals: BTreeMap<String, Counts> = BTreeMap::new();
    for score in results {
        for expected in &score.expected {
            let entry = totals.entry(expected.category.to_string()).or_default();
            if expected.caught {
                entry.tp += 1;
            } else {
                entry.missed += 1;
            }
        }
        for hit in &score.forbidden_hits {
            totals.entry(hit.category.to_string()).or_default().fp += 1;
        }
        for other in &score.unlabeled {
            totals.entry(other.category.to_string()).or_default().unlabeled += 1;
        }
    }
    totals
        .into_iter()
        .map(|(name, counts)| {
            let metrics =
                CategoryMetrics { counts, precision: counts.precision(), recall: counts.recall(), f1: counts.f1() };
            (name, metrics)
        })
        .collect()
}

fn summarize_cases(results: &[CaseScore]) -> Vec<CaseSummary> {
    let mut order: Vec<&str> = Vec::new();
    for score in results {
        if !order.contains(&score.case_id.as_str()) {
            order.push(&score.case_id);
        }
    }
    order
        .into_iter()
        .map(|id| summarize_case(id, &results.iter().filter(|s| s.case_id == id).collect::<Vec<_>>()))
        .collect()
}

fn summarize_case(case_id: &str, scores: &[&CaseScore]) -> CaseSummary {
    let runs = scores.len() as u32;
    let mut counts = Counts::default();
    let mut criteria = CriterionCounts::default();
    for score in scores {
        counts += score.counts();
        criteria += score.criterion_counts();
    }
    let first = scores[0];
    let expected = first
        .expected
        .iter()
        .enumerate()
        .map(|(index, outcome)| {
            let caught_runs = scores.iter().filter(|s| s.expected.get(index).is_some_and(|e| e.caught)).count() as u32;
            ExpectedSummary {
                label: outcome.label.clone(),
                category: outcome.category,
                caught_runs,
                caught: majority(caught_runs, runs),
            }
        })
        .collect();
    let criteria_detail = first
        .criteria
        .iter()
        .map(|outcome| {
            let correct_runs = scores
                .iter()
                .filter(|s| {
                    s.criteria.iter().any(|c| c.index == outcome.index && c.verdict == CriterionVerdict::Correct)
                })
                .count() as u32;
            CriterionSummary {
                index: outcome.index,
                text: outcome.text.clone(),
                expect: outcome.expect,
                correct_runs,
                correct: majority(correct_runs, runs),
            }
        })
        .collect();
    CaseSummary {
        case_id: case_id.to_string(),
        runs,
        counts,
        precision: counts.precision(),
        recall: counts.recall(),
        f1: counts.f1(),
        ac_accuracy: criteria.accuracy(),
        criteria,
        expected,
        criteria_detail,
        cost_usd: scores.iter().map(|s| s.cost_usd).sum(),
        duration_ms: scores.iter().map(|s| s.duration_ms).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::domain::fixtures::score;
    use crate::eval::domain::scoring::CriterionOutcome;
    use crate::review::application::CriterionStatus;

    fn criterion(index: usize, verdict: CriterionVerdict) -> CriterionOutcome {
        CriterionOutcome {
            index,
            text: format!("c{index}"),
            expect: ExpectedStatus::Met,
            actual: CriterionStatus::Met,
            verdict,
        }
    }

    #[test]
    fn pools_counts_and_derives_metrics() {
        let results = [score("a", 0, &[true, false], 1), score("b", 0, &[true], 0)];

        let metrics = aggregate(&results, 1);

        assert_eq!(metrics.counts, Counts { tp: 2, missed: 1, fp: 1, unlabeled: 0 });
        assert_eq!(metrics.case_count, 2);
        assert_eq!(metrics.precision.mean, Some(2.0 / 3.0));
        assert_eq!(metrics.recall.mean, Some(2.0 / 3.0));
        assert_eq!(metrics.cost_usd, 1.0);
        assert_eq!(metrics.duration_ms, 200);
        assert_eq!(metrics.f1.stddev, Some(0.0));
    }

    #[test]
    fn repetitions_give_mean_and_spread() {
        let results = [score("a", 0, &[true], 0), score("a", 1, &[false], 0)];

        let metrics = aggregate(&results, 2);

        assert_eq!(metrics.recall.mean, Some(0.5));
        assert_eq!(metrics.recall.stddev, Some(0.5));
        assert_eq!(metrics.recall.runs, 2);
        assert_eq!(metrics.cases[0].runs, 2);
    }

    #[test]
    fn an_expected_finding_is_caught_when_found_in_at_least_half_the_runs() {
        let results = [
            score("a", 0, &[true, false], 0),
            score("a", 1, &[false, false], 0),
            score("a", 2, &[true, true], 0),
            score("a", 3, &[false, true], 0),
        ];

        let summary = &aggregate(&results, 4).cases[0];

        assert_eq!((summary.expected[0].caught_runs, summary.expected[0].caught), (2, true));
        assert_eq!((summary.expected[1].caught_runs, summary.expected[1].caught), (2, true));
        let odd = aggregate(&results[..3], 3);
        assert!(!odd.cases[0].expected[1].caught);
    }

    #[test]
    fn criteria_accuracy_and_unverifiable_rate_follow_the_verdicts() {
        let mut result = score("a", 0, &[], 0);
        result.criteria = vec![criterion(0, CriterionVerdict::Correct), criterion(1, CriterionVerdict::Unclear)];

        let metrics = aggregate(&[result], 1);

        assert_eq!(metrics.ac_accuracy.mean, Some(0.5));
        assert_eq!(metrics.unverifiable_rate.mean, Some(0.5));
        let detail = &metrics.cases[0].criteria_detail;
        assert!(detail[0].correct && !detail[1].correct);
    }

    #[test]
    fn categories_break_down_catches_misses_and_false_positives() {
        let metrics = aggregate(&[score("a", 0, &[true, false], 2)], 1);

        let security = &metrics.by_category["security"];
        assert_eq!(security.counts, Counts { tp: 1, missed: 1, fp: 0, unlabeled: 0 });
        assert_eq!(security.recall, Some(0.5));
        assert_eq!(metrics.by_category["correctness"].counts.fp, 2);
        assert_eq!(metrics.by_category["correctness"].precision, Some(0.0));
    }

    #[test]
    fn nothing_to_measure_leaves_metrics_undefined() {
        let metrics = aggregate(&[], 1);

        assert_eq!(metrics.f1, Stat::default());
        assert_eq!(metrics.headline().ac_accuracy, None);
        assert!(metrics.cases.is_empty());
    }

    #[test]
    fn eval_run_new_computes_metrics_and_starts_unsaved() {
        let provenance = RunProvenance { model: "m".into(), claude_cli_version: "1".into(), prompt_hash: "h".into() };

        let run = EvalRun::new(chrono::Utc::now(), provenance, 1, false, vec![score("a", 0, &[true], 0)]);

        assert_eq!(run.id, 0);
        assert_eq!(run.metrics.headline().recall, Some(1.0));
    }
}
