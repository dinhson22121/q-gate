use serde::{Deserialize, Serialize};

use super::run::{EvalRun, Headline, aggregate};
use super::scoring::CaseScore;
use crate::history::application::RunProvenance;

pub const DEFAULT_MAX_DROP: f64 = 0.05;
const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Deltas {
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub f1: Option<f64>,
    pub ac_accuracy: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostCatch {
    pub case_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostCriterion {
    pub case_id: String,
    pub index: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub baseline_id: i64,
    pub max_drop: f64,
    pub current: Headline,
    pub baseline: Headline,
    pub deltas: Deltas,
    pub regressed: bool,
    pub regressions: Vec<String>,
    pub lost_catches: Vec<LostCatch>,
    pub lost_criteria: Vec<LostCriterion>,
    pub provenance_changes: Vec<String>,
    pub only_in_current: Vec<String>,
    pub only_in_baseline: Vec<String>,
    pub notes: Vec<String>,
}

pub fn compare(current: &EvalRun, baseline: &EvalRun, max_drop: f64) -> Comparison {
    let current_ids = case_ids(&current.results);
    let baseline_ids = case_ids(&baseline.results);
    let common: Vec<&str> = current_ids.iter().filter(|id| baseline_ids.contains(id)).copied().collect();
    let keep = |results: &[CaseScore]| -> Vec<CaseScore> {
        results.iter().filter(|r| common.contains(&r.case_id.as_str())).cloned().collect()
    };
    let current_metrics = aggregate(&keep(&current.results), current.runs_per_case);
    let baseline_metrics = aggregate(&keep(&baseline.results), baseline.runs_per_case);
    let (now, before) = (current_metrics.headline(), baseline_metrics.headline());
    let deltas = Deltas {
        precision: delta(now.precision, before.precision),
        recall: delta(now.recall, before.recall),
        f1: delta(now.f1, before.f1),
        ac_accuracy: delta(now.ac_accuracy, before.ac_accuracy),
    };
    let mut regressions = Vec::new();
    check_drop("F1", deltas.f1, before.f1, max_drop, &mut regressions);
    check_drop("AC accuracy", deltas.ac_accuracy, before.ac_accuracy, max_drop, &mut regressions);
    let lost_catches = lost_catches(&current_metrics.cases, &baseline_metrics.cases);
    let lost_criteria = lost_criteria(&current_metrics.cases, &baseline_metrics.cases);
    if !lost_catches.is_empty() {
        regressions.push(format!("{} finding(s) caught in the baseline are no longer caught", lost_catches.len()));
    }
    if !lost_criteria.is_empty() {
        regressions.push(format!("{} criterion verdict(s) correct in the baseline are now wrong", lost_criteria.len()));
    }
    Comparison {
        baseline_id: baseline.id,
        max_drop,
        current: now,
        baseline: before,
        deltas,
        regressed: !regressions.is_empty(),
        regressions,
        lost_catches,
        lost_criteria,
        provenance_changes: provenance_changes(&current.provenance, &baseline.provenance),
        only_in_current: difference(&current_ids, &baseline_ids),
        only_in_baseline: difference(&baseline_ids, &current_ids),
        notes: notes(current, baseline, common.is_empty()),
    }
}

fn case_ids(results: &[CaseScore]) -> Vec<&str> {
    let mut ids: Vec<&str> = Vec::new();
    for result in results {
        if !ids.contains(&result.case_id.as_str()) {
            ids.push(&result.case_id);
        }
    }
    ids
}

fn difference(left: &[&str], right: &[&str]) -> Vec<String> {
    left.iter().filter(|id| !right.contains(id)).map(ToString::to_string).collect()
}

fn delta(now: Option<f64>, before: Option<f64>) -> Option<f64> {
    Some(now? - before?)
}

fn check_drop(name: &str, delta: Option<f64>, before: Option<f64>, max_drop: f64, regressions: &mut Vec<String>) {
    let (Some(delta), Some(before)) = (delta, before) else { return };
    if -delta > max_drop + EPSILON {
        regressions.push(format!(
            "{name} dropped {:.3} (from {before:.3} to {:.3}), more than the allowed {max_drop:.3}",
            -delta,
            before + delta
        ));
    }
}

fn lost_catches(current: &[super::run::CaseSummary], baseline: &[super::run::CaseSummary]) -> Vec<LostCatch> {
    let mut lost = Vec::new();
    for before in baseline {
        let Some(now) = current.iter().find(|c| c.case_id == before.case_id) else { continue };
        for expected in before.expected.iter().filter(|e| e.caught) {
            if now.expected.iter().any(|e| e.label == expected.label && !e.caught) {
                lost.push(LostCatch { case_id: before.case_id.clone(), label: expected.label.clone() });
            }
        }
    }
    lost
}

fn lost_criteria(current: &[super::run::CaseSummary], baseline: &[super::run::CaseSummary]) -> Vec<LostCriterion> {
    let mut lost = Vec::new();
    for before in baseline {
        let Some(now) = current.iter().find(|c| c.case_id == before.case_id) else { continue };
        for criterion in before.criteria_detail.iter().filter(|c| c.correct) {
            let regressed = now
                .criteria_detail
                .iter()
                .any(|c| c.index == criterion.index && c.text == criterion.text && !c.correct);
            if regressed {
                lost.push(LostCriterion {
                    case_id: before.case_id.clone(),
                    index: criterion.index,
                    text: criterion.text.clone(),
                });
            }
        }
    }
    lost
}

fn provenance_changes(current: &RunProvenance, baseline: &RunProvenance) -> Vec<String> {
    let mut changes = Vec::new();
    let mut note = |name: &str, now: &str, before: &str| {
        if now != before {
            changes.push(format!("{name} changed from {before} to {now}"));
        }
    };
    note("model", &current.model, &baseline.model);
    note("claude CLI version", &current.claude_cli_version, &baseline.claude_cli_version);
    note("prompt hash", short(&current.prompt_hash), short(&baseline.prompt_hash));
    changes
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn notes(current: &EvalRun, baseline: &EvalRun, no_common_cases: bool) -> Vec<String> {
    let mut notes = Vec::new();
    if no_common_cases {
        notes.push("no cases in common with the baseline; nothing could be compared".to_string());
    }
    if current.runs_per_case != baseline.runs_per_case {
        notes.push(format!(
            "repetitions differ: {} now, {} in the baseline",
            current.runs_per_case, baseline.runs_per_case
        ));
    }
    if current.with_tests != baseline.with_tests {
        notes.push("the test agent was enabled in only one of the two runs".to_string());
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::domain::case::ExpectedStatus;
    use crate::eval::domain::fixtures::score;
    use crate::eval::domain::scoring::{CriterionOutcome, CriterionVerdict};
    use crate::review::application::CriterionStatus;

    fn provenance(model: &str) -> RunProvenance {
        RunProvenance { model: model.into(), claude_cli_version: "1.0".into(), prompt_hash: "h".repeat(64) }
    }

    fn run(id: i64, results: Vec<CaseScore>) -> EvalRun {
        EvalRun { id, ..EvalRun::new(chrono::Utc::now(), provenance("m"), 1, false, results) }
    }

    fn with_criterion(mut score: CaseScore, verdict: CriterionVerdict) -> CaseScore {
        score.criteria = vec![CriterionOutcome {
            index: 0,
            text: "c".into(),
            expect: ExpectedStatus::Met,
            actual: CriterionStatus::Met,
            verdict,
        }];
        score
    }

    #[test]
    fn identical_runs_do_not_regress() {
        let results = vec![score("a", 0, &[true, true], 0)];

        let comparison = compare(&run(2, results.clone()), &run(1, results), DEFAULT_MAX_DROP);

        assert!(!comparison.regressed && comparison.regressions.is_empty());
        assert_eq!(comparison.deltas.f1, Some(0.0));
        assert_eq!(comparison.baseline_id, 1);
        assert!(comparison.provenance_changes.is_empty() && comparison.notes.is_empty());
    }

    #[test]
    fn an_f1_drop_is_judged_against_max_drop() {
        let baseline = run(1, vec![score("a", 0, &[true; 4], 0)]);
        let current = run(2, vec![score("a", 0, &[true; 4], 1)]);
        let drop = baseline.metrics.headline().f1.unwrap() - current.metrics.headline().f1.unwrap();

        let strict = compare(&current, &baseline, 0.05);
        let lenient = compare(&current, &baseline, 0.12);
        let exactly_at_limit = compare(&current, &baseline, drop);

        assert!(strict.regressed && strict.regressions[0].starts_with("F1 dropped"));
        assert!(!lenient.regressed);
        assert!(!exactly_at_limit.regressed, "a drop equal to max_drop is allowed");
        assert!(strict.lost_catches.is_empty());
    }

    #[test]
    fn a_lost_catch_is_listed_even_when_f1_holds() {
        let baseline = run(1, vec![score("a", 0, &[true, false], 0), score("b", 0, &[false, true], 0)]);
        let current = run(2, vec![score("a", 0, &[false, true], 0), score("b", 0, &[true, false], 0)]);

        let comparison = compare(&current, &baseline, 0.5);

        assert!(comparison.regressed);
        assert_eq!(comparison.deltas.f1, Some(0.0));
        assert_eq!(comparison.lost_catches.len(), 2);
        assert_eq!(comparison.lost_catches[0].case_id, "a");
        assert_eq!(comparison.lost_catches[0].label, "a.go:0 security");
    }

    #[test]
    fn a_criterion_that_was_right_and_is_now_wrong_regresses() {
        let baseline = run(1, vec![with_criterion(score("a", 0, &[], 0), CriterionVerdict::Correct)]);
        let current = run(2, vec![with_criterion(score("a", 0, &[], 0), CriterionVerdict::Incorrect)]);

        let comparison = compare(&current, &baseline, 1.0);

        assert!(comparison.regressed);
        assert_eq!(comparison.lost_criteria, [LostCriterion { case_id: "a".into(), index: 0, text: "c".into() }]);
        assert_eq!(comparison.deltas.ac_accuracy, Some(-1.0));
    }

    #[test]
    fn an_improvement_is_not_a_regression() {
        let baseline = run(1, vec![score("a", 0, &[false, true], 1)]);
        let current = run(2, vec![score("a", 0, &[true, true], 0)]);

        let comparison = compare(&current, &baseline, DEFAULT_MAX_DROP);

        assert!(!comparison.regressed);
        assert!(comparison.deltas.f1.unwrap() > 0.0);
    }

    #[test]
    fn cases_present_on_one_side_only_are_excluded_and_listed() {
        let baseline = run(1, vec![score("a", 0, &[true], 0), score("old", 0, &[true], 0)]);
        let current = run(2, vec![score("a", 0, &[true], 0), score("new", 0, &[false], 0)]);

        let comparison = compare(&current, &baseline, DEFAULT_MAX_DROP);

        assert!(!comparison.regressed);
        assert_eq!(comparison.only_in_current, ["new"]);
        assert_eq!(comparison.only_in_baseline, ["old"]);
        assert_eq!(comparison.current.recall, Some(1.0));
    }

    #[test]
    fn no_common_cases_is_noted_not_failed() {
        let comparison =
            compare(&run(2, vec![score("a", 0, &[true], 0)]), &run(1, vec![score("b", 0, &[true], 0)]), 0.05);

        assert!(!comparison.regressed);
        assert!(comparison.notes[0].contains("no cases in common"));
    }

    #[test]
    fn provenance_and_setup_differences_are_noted() {
        let mut current = run(2, vec![score("a", 0, &[true], 0)]);
        current.provenance = RunProvenance { claude_cli_version: "2.0".into(), ..provenance("other") };
        current.provenance.prompt_hash = "p".repeat(64);
        current.runs_per_case = 3;
        current.with_tests = true;
        let baseline = run(1, vec![score("a", 0, &[true], 0)]);

        let comparison = compare(&current, &baseline, 0.05);

        assert_eq!(comparison.provenance_changes.len(), 3);
        assert!(comparison.provenance_changes[0].contains("model changed from m to other"));
        assert_eq!(comparison.notes.len(), 2);
    }

    #[test]
    fn undefined_metrics_never_regress() {
        let comparison = compare(&run(2, vec![score("a", 0, &[], 0)]), &run(1, vec![score("a", 0, &[], 0)]), 0.0);

        assert!(!comparison.regressed);
        assert_eq!(comparison.deltas.f1, None);
    }
}
