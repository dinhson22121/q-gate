//! Terminal and JSON presentation of a review report.

use std::fmt::Write as _;

use owo_colors::OwoColorize;
use serde_json::json;

use crate::review::application::{
    CheckOutcome, CheckResult, Confidence, CriterionStatus, Finding, RequirementsReport, ReviewReport, Severity,
};

/// Output tail lines shown for a failed check.
const CHECK_TAIL_LINES: usize = 10;
const TOKENS_PER_THOUSAND: f64 = 1000.0;

/// Which non-blocking findings to show (blocking ones are always shown).
#[derive(Debug, Clone, Copy)]
pub struct DisplayFilter {
    pub min_severity: Severity,
    pub min_confidence: Confidence,
}

pub struct TerminalView<'a> {
    pub report: &'a ReviewReport,
    pub filter: DisplayFilter,
    pub ticket_note: Option<&'a str>,
    pub color: bool,
}

pub fn render_json(report: &ReviewReport, ticket_note: Option<&str>, run_id: Option<i64>) -> String {
    let value = json!({ "report": report, "ticket_note": ticket_note, "run_id": run_id });
    serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

pub fn render_terminal(view: &TerminalView<'_>) -> String {
    let mut out = String::new();
    let report = view.report;
    let _ = writeln!(
        out,
        "{} reviewed {} file(s), {} skipped",
        paint("qgate:", Tone::Bold, view.color),
        report.files_reviewed.len(),
        report.skipped.len()
    );
    render_requirements(&mut out, report.requirements.as_ref(), view);
    render_checks(&mut out, &report.checks, view.color);
    render_findings(&mut out, view);
    render_skipped(&mut out, report);
    render_verdict(&mut out, report, view.color);
    out
}

fn render_requirements(out: &mut String, requirements: Option<&RequirementsReport>, view: &TerminalView<'_>) {
    if let Some(note) = view.ticket_note {
        let _ = writeln!(out, "\n{} {note}", paint("Acceptance criteria:", Tone::Bold, view.color));
    }
    let Some(req) = requirements else { return };
    let _ = writeln!(
        out,
        "\n{} {} {}",
        paint("Acceptance criteria", Tone::Bold, view.color),
        req.requirements.key,
        req.requirements.title
    );
    for (criterion, assessment) in req.requirements.criteria.iter().zip(&req.assessments) {
        let (symbol, tone) = match assessment.status {
            CriterionStatus::Met => ("✔ met     ", Tone::Good),
            CriterionStatus::NotMet => ("✘ not met ", Tone::Bad),
            CriterionStatus::Unclear => ("? unclear ", Tone::Warn),
        };
        let _ = writeln!(out, "  {} {} {}", paint(symbol, tone, view.color), criterion.id, criterion.text);
        let _ = writeln!(out, "      {}", assessment.explanation);
        for evidence in &assessment.evidence {
            let _ = writeln!(out, "      {}:{}  {}", evidence.file, evidence.line, evidence.quote.trim());
        }
    }
}

fn render_checks(out: &mut String, checks: &[CheckResult], color: bool) {
    if checks.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{}", paint("Checks", Tone::Bold, color));
    for check in checks {
        let (symbol, tone) = match check.outcome {
            CheckOutcome::Passed => ("✔", Tone::Good),
            CheckOutcome::Failed | CheckOutcome::TimedOut | CheckOutcome::Error => {
                if check.required {
                    ("✘", Tone::Bad)
                } else {
                    ("!", Tone::Warn)
                }
            }
        };
        let optional = if check.required { "" } else { " (optional)" };
        let _ = writeln!(
            out,
            "  {} {}{optional}  {:?}  {:.1}s",
            paint(symbol, tone, color),
            check.name,
            check.outcome,
            check.duration_ms as f64 / 1000.0
        );
        if check.outcome != CheckOutcome::Passed {
            let lines: Vec<&str> = check.output_tail.lines().collect();
            for line in &lines[lines.len().saturating_sub(CHECK_TAIL_LINES)..] {
                let _ = writeln!(out, "      {line}");
            }
        }
    }
}

fn render_findings(out: &mut String, view: &TerminalView<'_>) {
    let report = view.report;
    let visible: Vec<(usize, &Finding)> = report
        .findings
        .iter()
        .enumerate()
        .filter(|(i, f)| {
            report.gate.blocking.contains(i)
                || (f.severity >= view.filter.min_severity && f.confidence >= view.filter.min_confidence)
        })
        .collect();
    if !visible.is_empty() {
        let _ = writeln!(out, "\n{}", paint("Findings", Tone::Bold, view.color));
    }
    let mut current_file = "";
    for (index, finding) in &visible {
        if finding.file != current_file {
            current_file = &finding.file;
            let _ = writeln!(out, "  {}", paint(current_file, Tone::Bold, view.color));
        }
        render_finding(out, finding, report.gate.blocking.contains(index), view.color);
    }
    let hidden = report.findings.len() - visible.len();
    if hidden > 0 {
        let _ = writeln!(
            out,
            "  ({hidden} lower-severity/confidence finding(s) hidden; see --format json or the dashboard)"
        );
    }
}

fn render_finding(out: &mut String, finding: &Finding, blocking: bool, color: bool) {
    let tone = match finding.severity {
        Severity::Critical => Tone::Bad,
        Severity::Major => Tone::Warn,
        Severity::Minor | Severity::Info => Tone::Dim,
    };
    let label = finding.severity.as_str().to_uppercase();
    let blocking = if blocking { paint(" BLOCKING", Tone::Bad, color) } else { String::new() };
    let _ = writeln!(
        out,
        "    {} L{} {} [{}, {} confidence]{blocking}",
        paint(&label, tone, color),
        finding.line,
        finding.title,
        finding.category,
        finding.confidence
    );
    let _ = writeln!(out, "      {}", finding.explanation);
    for line in finding.evidence.lines() {
        let _ = writeln!(out, "      │ {line}");
    }
    if let Some(suggestion) = &finding.suggestion {
        let _ = writeln!(out, "      fix: {suggestion}");
    }
}

fn render_skipped(out: &mut String, report: &ReviewReport) {
    for skipped in &report.skipped {
        let _ = writeln!(out, "  skipped {} ({})", skipped.path, skipped.reason.describe());
    }
    if !report.rejected.is_empty() {
        let _ = writeln!(
            out,
            "  {} unverified finding(s) dropped: the quoted code was not found in the change",
            report.rejected.len()
        );
    }
}

fn render_verdict(out: &mut String, report: &ReviewReport, color: bool) {
    let gate = &report.gate;
    let mut reasons = Vec::new();
    if !gate.blocking.is_empty() {
        reasons.push(format!("{} blocking finding(s)", gate.blocking.len()));
    }
    if !gate.unmet_criteria.is_empty() {
        reasons.push(format!("unmet criteria: {}", gate.unmet_criteria.join(", ")));
    }
    if !gate.failed_checks.is_empty() {
        reasons.push(format!("failed checks: {}", gate.failed_checks.join(", ")));
    }
    if gate.incomplete {
        reasons.push("review incomplete (some files could not be reviewed)".into());
    }
    let verdict = if gate.passed {
        paint("PASSED", Tone::Good, color)
    } else {
        format!("{} — {}", paint("FAILED", Tone::Bad, color), reasons.join("; "))
    };
    let _ = writeln!(out, "\nGate: {verdict}");
    if report.is_incomplete() {
        let _ = writeln!(
            out,
            "{}",
            paint("Warning: some files could not be reviewed (see skipped above).", Tone::Warn, color)
        );
    }
    let usage = &report.usage;
    let cost = report.estimated_cost_usd.map(|c| format!(" · est. ${c:.2}")).unwrap_or_default();
    let _ = writeln!(
        out,
        "Tokens: {:.1}k in / {:.1}k out (cache read {:.1}k){cost}",
        usage.input_tokens as f64 / TOKENS_PER_THOUSAND,
        usage.output_tokens as f64 / TOKENS_PER_THOUSAND,
        usage.cache_read_tokens as f64 / TOKENS_PER_THOUSAND
    );
}

#[derive(Clone, Copy)]
enum Tone {
    Bold,
    Good,
    Bad,
    Warn,
    Dim,
}

fn paint(text: &str, tone: Tone, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    match tone {
        Tone::Bold => text.bold().to_string(),
        Tone::Good => text.green().bold().to_string(),
        Tone::Bad => text.red().bold().to_string(),
        Tone::Warn => text.yellow().to_string(),
        Tone::Dim => text.dimmed().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::application::{AcceptanceCriterion, Category, Requirements, TokenUsage};
    use crate::review::domain::finding::FindingSource;
    use crate::review::domain::gate::GateDecision;
    use crate::review::domain::requirements::CriterionAssessment;

    fn finding(severity: Severity, confidence: Confidence) -> Finding {
        Finding {
            file: "api.go".into(),
            line: 42,
            severity,
            category: Category::Security,
            confidence,
            title: "SQL built from input".into(),
            explanation: "id is concatenated".into(),
            evidence: "q := \"SELECT \" + id".into(),
            suggestion: Some("use placeholders".into()),
            prevention_rule: None,
            source: FindingSource::Llm,
        }
    }

    fn report() -> ReviewReport {
        ReviewReport {
            files_reviewed: vec!["api.go".into()],
            skipped: vec![],
            findings: vec![finding(Severity::Critical, Confidence::High), finding(Severity::Info, Confidence::Low)],
            rejected: vec![],
            requirements: Some(RequirementsReport {
                requirements: Requirements {
                    key: "SHOP-7".into(),
                    title: "Export".into(),
                    url: None,
                    criteria: vec![AcceptanceCriterion { id: "AC-1".into(), text: "Export CSV".into() }],
                },
                assessments: vec![CriterionAssessment {
                    criterion_id: "AC-1".into(),
                    status: CriterionStatus::NotMet,
                    explanation: "no export endpoint".into(),
                    evidence: vec![],
                }],
            }),
            checks: vec![CheckResult {
                name: "unit".into(),
                required: true,
                outcome: CheckOutcome::Failed,
                exit_code: Some(1),
                duration_ms: 1500,
                output_tail: "--- FAIL: TestExport".into(),
            }],
            agents: None,
            ticket: None,
            gate: GateDecision {
                passed: false,
                blocking: vec![0],
                unmet_criteria: vec!["AC-1".into()],
                failed_checks: vec!["unit".into()],
                incomplete: false,
                ..GateDecision::default()
            },
            usage: TokenUsage { input_tokens: 12_300, output_tokens: 2_100, ..TokenUsage::default() },
            estimated_cost_usd: Some(0.09),
        }
    }

    #[test]
    fn terminal_output_shows_criteria_checks_blocking_findings_and_verdict() {
        let report = report();
        let view = TerminalView {
            report: &report,
            filter: DisplayFilter { min_severity: Severity::Minor, min_confidence: Confidence::Medium },
            ticket_note: None,
            color: false,
        };
        let text = render_terminal(&view);
        assert!(text.contains("✘ not met  AC-1 Export CSV"));
        assert!(text.contains("--- FAIL: TestExport"));
        assert!(text.contains("CRITICAL L42 SQL built from input [security, high confidence] BLOCKING"));
        assert!(text.contains("1 lower-severity/confidence finding(s) hidden"));
        assert!(text.contains("FAILED — 1 blocking finding(s); unmet criteria: AC-1; failed checks: unit"));
        assert!(text.contains("est. $0.09"));
        assert!(!text.contains('\u{1b}'), "no ANSI codes without color");
    }

    #[test]
    fn colored_output_contains_ansi_codes() {
        let report = report();
        let view = TerminalView {
            report: &report,
            filter: DisplayFilter { min_severity: Severity::Info, min_confidence: Confidence::Low },
            ticket_note: Some("Jira: unavailable"),
            color: true,
        };
        let text = render_terminal(&view);
        assert!(text.contains('\u{1b}'));
        assert!(text.contains("Jira: unavailable"));
    }

    #[test]
    fn json_wraps_report_with_note_and_run_id() {
        let value: serde_json::Value = serde_json::from_str(&render_json(&report(), Some("n"), Some(3))).unwrap();
        assert_eq!(value["run_id"], 3);
        assert_eq!(value["ticket_note"], "n");
        assert_eq!(value["report"]["gate"]["passed"], false);
    }
}
