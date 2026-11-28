use std::fmt::Write;

use crate::eval::domain::compare::Comparison;
use crate::eval::domain::run::{CaseSummary, EvalRun};
use crate::eval::domain::stats::Stat;
use crate::eval::domain::test_outcome::TestsOutcome;

const CASE_WIDTH: usize = 34;

fn pct(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |v| format!("{v:.3}"))
}

fn delta(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |v| format!("{v:+.3}"))
}

fn stat(stat: &Stat) -> String {
    match (stat.mean, stat.stddev) {
        (Some(mean), Some(stddev)) if stat.runs > 1 => format!("{mean:.3} ± {stddev:.3}"),
        (Some(mean), _) => format!("{mean:.3}"),
        _ => "-".to_string(),
    }
}

fn criteria_cell(case: &CaseSummary) -> String {
    let c = case.criteria;
    if c.total == 0 { "-".into() } else { format!("{}/{}", c.correct, c.total) }
}

pub fn render_run(run: &EvalRun, comparison: Option<&Comparison>) -> String {
    let mut out = String::new();
    let p = &run.provenance;
    let _ = writeln!(
        out,
        "Eval run #{} · {} · model {} · claude {} · prompt {} · {} run(s)/case · tests {}",
        run.id,
        run.created_at.format("%Y-%m-%d %H:%M UTC"),
        p.model,
        p.claude_cli_version,
        p.prompt_hash.get(..12).unwrap_or(&p.prompt_hash),
        run.runs_per_case,
        if run.with_tests { "on" } else { "off" },
    );
    let _ = writeln!(
        out,
        "\n{:<CASE_WIDTH$} {:>4} {:>3} {:>3} {:>3} {:>7} {:>6} {:>6} {:>6} {:>6} {:>8}",
        "CASE", "RUNS", "TP", "FN", "FP", "UNLABEL", "PREC", "RECALL", "F1", "AC", "COST"
    );
    for case in &run.metrics.cases {
        let c = case.counts;
        let _ = writeln!(
            out,
            "{:<CASE_WIDTH$} {:>4} {:>3} {:>3} {:>3} {:>7} {:>6} {:>6} {:>6} {:>6} {:>8}",
            case.case_id,
            case.runs,
            c.tp,
            c.missed,
            c.fp,
            c.unlabeled,
            pct(case.precision),
            pct(case.recall),
            pct(case.f1),
            criteria_cell(case),
            format!("${:.2}", case.cost_usd),
        );
    }
    let m = &run.metrics;
    let _ = writeln!(out, "\nprecision          {}", stat(&m.precision));
    let _ = writeln!(out, "recall             {}", stat(&m.recall));
    let _ = writeln!(out, "F1                 {}", stat(&m.f1));
    let _ = writeln!(out, "AC accuracy        {}", stat(&m.ac_accuracy));
    let _ = writeln!(out, "unverifiable rate  {}", stat(&m.unverifiable_rate));
    let _ = writeln!(
        out,
        "criteria           {} correct, {} incorrect, {} unclear of {}",
        m.criteria.correct, m.criteria.incorrect, m.criteria.unclear, m.criteria.total
    );
    let _ = writeln!(
        out,
        "findings           {} TP, {} FN, {} FP, {} unlabeled (not scored)",
        m.counts.tp, m.counts.missed, m.counts.fp, m.counts.unlabeled
    );
    if !m.by_category.is_empty() {
        let _ = writeln!(out, "\nby category        TP  FN  FP  UNLABEL  PREC   RECALL  F1");
        for (name, category) in &m.by_category {
            let c = category.counts;
            let _ = writeln!(
                out,
                "{name:<18} {:>2}  {:>2}  {:>2}  {:>7}  {:>5}  {:>6}  {:>5}",
                c.tp,
                c.missed,
                c.fp,
                c.unlabeled,
                pct(category.precision),
                pct(category.recall),
                pct(category.f1)
            );
        }
    }
    render_false_positives(&mut out, run);
    render_tests(&mut out, run);
    let _ = writeln!(
        out,
        "\ncost ${:.2} (this consumed Claude subscription quota) · {:.1}s of review time",
        m.cost_usd,
        m.duration_ms as f64 / 1000.0
    );
    if let Some(comparison) = comparison {
        out.push_str(&render_comparison(comparison));
    }
    out
}

fn render_tests(out: &mut String, run: &EvalRun) {
    if !run.with_tests {
        return;
    }
    let _ = writeln!(out, "\ntest agent");
    for result in &run.results {
        let summary = result.tests.as_ref().map_or_else(|| "no outcome recorded".to_string(), TestsOutcome::summary);
        let _ = writeln!(out, "  {} run {}: {summary}", result.case_id, result.repetition + 1);
    }
}

fn render_false_positives(out: &mut String, run: &EvalRun) {
    let hits: Vec<_> = run.results.iter().flat_map(|r| r.forbidden_hits.iter().map(move |f| (r, f))).collect();
    if hits.is_empty() {
        return;
    }
    let _ = writeln!(out, "\nfalse positives (forbidden hits)");
    for (result, finding) in hits {
        let _ = writeln!(
            out,
            "  {} run {}: {}:{} {} {} {}",
            result.case_id,
            result.repetition + 1,
            finding.file,
            finding.line,
            finding.severity.as_str(),
            finding.category.as_str(),
            finding.title
        );
    }
}

pub fn render_comparison(c: &Comparison) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nvs baseline #{}: F1 {} · precision {} · recall {} · AC accuracy {} (allowed drop {:.3})",
        c.baseline_id,
        delta(c.deltas.f1),
        delta(c.deltas.precision),
        delta(c.deltas.recall),
        delta(c.deltas.ac_accuracy),
        c.max_drop
    );
    for change in &c.provenance_changes {
        let _ = writeln!(out, "  note: {change}");
    }
    for note in &c.notes {
        let _ = writeln!(out, "  note: {note}");
    }
    if !c.only_in_current.is_empty() {
        let _ = writeln!(out, "  not in the baseline (excluded): {}", c.only_in_current.join(", "));
    }
    if !c.only_in_baseline.is_empty() {
        let _ = writeln!(out, "  only in the baseline (excluded): {}", c.only_in_baseline.join(", "));
    }
    if !c.changed_cases.is_empty() {
        let _ = writeln!(out, "  case content changed (excluded): {}", c.changed_cases.join(", "));
    }
    if !c.regressed {
        let _ = writeln!(out, "no regression");
        return out;
    }
    let _ = writeln!(out, "REGRESSION");
    for reason in &c.regressions {
        let _ = writeln!(out, "  - {reason}");
    }
    for lost in &c.lost_catches {
        let _ = writeln!(out, "  lost catch: {} · {}", lost.case_id, lost.label);
    }
    for lost in &c.lost_criteria {
        let _ = writeln!(out, "  lost criterion: {} · AC-{} · {}", lost.case_id, lost.index + 1, lost.text);
    }
    out
}

pub fn render_list(runs: &[EvalRun]) -> String {
    if runs.is_empty() {
        return "No eval runs yet. Run `qgate eval`.\n".into();
    }
    let mut out = format!(
        "{:>4}  {:<17} {:<24} {:>5} {:>6} {:>6} {:>8}  {:<13} {}\n",
        "ID", "WHEN", "MODEL", "CASES", "F1", "AC", "COST", "PROMPT", "SCOPE"
    );
    for run in runs {
        let _ = writeln!(
            out,
            "{:>4}  {:<17} {:<24} {:>5} {:>6} {:>6} {:>8}  {:<13} {}",
            run.id,
            run.created_at.format("%Y-%m-%d %H:%M"),
            run.provenance.model,
            run.metrics.case_count,
            pct(run.metrics.f1.mean),
            pct(run.metrics.ac_accuracy.mean),
            format!("${:.2}", run.metrics.cost_usd),
            run.provenance.prompt_hash.get(..12).unwrap_or(&run.provenance.prompt_hash),
            if run.partial { "partial" } else { "full" },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::domain::compare::compare;
    use crate::eval::domain::fixtures::score;
    use crate::history::application::RunProvenance;

    fn run(id: i64, runs: Vec<crate::eval::domain::scoring::CaseScore>, runs_per_case: u32) -> EvalRun {
        let provenance =
            RunProvenance { model: "claude-x".into(), claude_cli_version: "2.0".into(), prompt_hash: "ab".repeat(32) };
        EvalRun { id, ..EvalRun::new(chrono::Utc::now(), provenance, runs_per_case, false, runs) }
    }

    #[test]
    fn a_comparison_lists_cases_whose_content_changed() {
        let mut changed = score("alpha", 0, &[true], 0);
        changed.content_hash = "other".into();
        let baseline = run(1, vec![score("alpha", 0, &[true], 0)], 1);
        let current = run(2, vec![changed], 1);

        let text = render_comparison(&compare(&current, &baseline, 0.05));

        assert!(text.contains("case content changed (excluded): alpha"), "{text}");
    }

    #[test]
    fn the_report_shows_cases_totals_categories_and_cost() {
        let current = run(3, vec![score("alpha", 0, &[true, false], 1)], 1);

        let text = render_run(&current, None);

        assert!(
            text.contains("Eval run #3") && text.contains("model claude-x") && text.contains("prompt abababababab")
        );
        let row = text.lines().find(|l| l.starts_with("alpha")).unwrap();
        assert!(row.contains("0.500") && row.contains("$0.50"), "{row}");
        assert!(text.contains("precision          0.500"));
        assert!(text.contains("by category") && text.contains("security"));
        assert!(text.contains("consumed Claude subscription quota"));
        assert!(!text.contains("REGRESSION"));
    }

    #[test]
    fn tests_section_lists_ran_and_skipped_cases() {
        use crate::eval::domain::test_outcome::TestsOutcome;
        let mut ran = score("alpha", 0, &[true], 0);
        ran.tests = Some(TestsOutcome { ran: true, scenarios: 2, passed: 2, cost_usd: 0.3, ..TestsOutcome::default() });
        let mut skipped = score("beta", 0, &[true], 0);
        skipped.tests = Some(TestsOutcome { notes: vec!["no go module".into()], ..TestsOutcome::default() });
        let old = score("gamma", 0, &[true], 0);
        let mut current = run(1, vec![ran, skipped, old], 1);
        current.with_tests = true;

        let text = render_run(&current, None);

        assert!(text.contains("alpha run 1: ran 2 scenario(s): 2 passed, 0 failed, 0 inconclusive"), "{text}");
        assert!(text.contains("beta run 1: skipped: no go module"), "{text}");
        assert!(text.contains("gamma run 1: no outcome recorded"), "{text}");
        assert!(!render_run(&run(2, vec![score("a", 0, &[true], 0)], 1), None).contains("test agent"));
    }

    #[test]
    fn false_positives_are_listed_with_their_title() {
        let mut hit = score("beta", 0, &[], 1);
        hit.forbidden_hits[0].title = "Argument injection into convert".into();
        let clean = run(4, vec![score("alpha", 0, &[true], 0)], 1);

        let text = render_run(&run(5, vec![hit], 1), None);

        assert!(text.contains("false positives (forbidden hits)"));
        assert!(text.contains("beta run 1:") && text.contains("Argument injection into convert"), "{text}");
        assert!(!render_run(&clean, None).contains("false positives"));
    }

    #[test]
    fn repetitions_show_the_spread() {
        let current = run(1, vec![score("a", 0, &[true], 0), score("a", 1, &[false], 0)], 2);

        assert!(render_run(&current, None).contains("recall             0.500 ± 0.500"));
    }

    #[test]
    fn a_regression_is_spelled_out() {
        let baseline = run(1, vec![score("a", 0, &[true, true], 0), score("gone", 0, &[true], 0)], 1);
        let current = run(2, vec![score("a", 0, &[true, false], 0), score("new", 0, &[true], 0)], 1);
        let comparison = compare(&current, &baseline, 0.05);

        let text = render_run(&current, Some(&comparison));

        assert!(text.contains("vs baseline #1") && text.contains("REGRESSION"));
        assert!(text.contains("lost catch: a · a.go:1 security"));
        assert!(text.contains("not in the baseline (excluded): new"));
        assert!(text.contains("only in the baseline (excluded): gone"));
    }

    #[test]
    fn no_regression_and_provenance_notes() {
        let baseline = run(1, vec![score("a", 0, &[true], 0)], 1);
        let mut current = run(2, vec![score("a", 0, &[true], 0)], 1);
        current.provenance.model = "other".into();

        let text = render_comparison(&compare(&current, &baseline, 0.05));

        assert!(text.contains("no regression") && text.contains("note: model changed from claude-x to other"));
    }

    #[test]
    fn criteria_columns_and_undefined_metrics_use_dashes() {
        let mut result = score("a", 0, &[], 0);
        result.criteria = vec![crate::eval::domain::scoring::CriterionOutcome {
            index: 0,
            text: "c".into(),
            expect: crate::eval::domain::case::ExpectedStatus::Met,
            actual: crate::review::application::CriterionStatus::Met,
            verdict: crate::eval::domain::scoring::CriterionVerdict::Correct,
        }];

        let text = render_run(&run(1, vec![result], 1), None);

        let row = text.lines().find(|l| l.starts_with("a ")).unwrap();
        assert!(row.contains("1/1") && row.contains(" - "), "{row}");
    }

    #[test]
    fn the_list_has_one_row_per_run_or_a_hint() {
        assert!(render_list(&[]).contains("qgate eval"));

        let text =
            render_list(&[run(2, vec![score("a", 0, &[true], 0)], 1), run(1, vec![score("a", 0, &[true], 0)], 1)]);

        assert_eq!(text.lines().count(), 3);
        assert!(text.lines().nth(1).unwrap().trim_start().starts_with('2'));
    }

    #[test]
    fn the_list_marks_partial_runs() {
        let partial = run(2, vec![score("a", 0, &[true], 0)], 1).with_partial(true);

        let text = render_list(&[partial, run(1, vec![score("a", 0, &[true], 0)], 1)]);

        assert!(text.lines().nth(1).unwrap().ends_with("partial"));
        assert!(text.lines().nth(2).unwrap().ends_with("full"));
    }
}
