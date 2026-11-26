use std::fmt::Write as _;

use super::document::{HandoffDocument, HandoffInput};
use crate::review::application::{
    AgentReport, CheckOutcome, CheckResult, CriterionStatus, DesignMismatch, EvidenceRef, Finding, ReviewReport,
    RuleViolation, Severity, TestResult, TestVerdict,
};
use crate::review::domain::finding::FindingSource;

const STYLE: &str = "\
:root{--ok:#15803d;--bad:#b91c1c;--warn:#b45309;--mute:#6b7280;--line:#e5e7eb}\
*{box-sizing:border-box}\
body{font:14px/1.5 -apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#111827;max-width:960px;margin:0 auto;padding:24px}\
h1{font-size:22px;margin:0 0 4px}h2{font-size:17px;margin:28px 0 10px;border-bottom:2px solid var(--line);padding-bottom:4px}\
h3{font-size:14px;margin:0 0 6px}\
.verdict{display:inline-block;font-size:28px;font-weight:800;letter-spacing:.05em;color:#fff;padding:8px 28px;border-radius:8px}\
.pass{background:var(--ok)}.fail{background:var(--bad)}\
.meta{width:100%;border-collapse:collapse;margin:12px 0}.meta th{text-align:left;width:140px;color:var(--mute);font-weight:600;vertical-align:top;padding:3px 8px 3px 0}.meta td{padding:3px 0}\
.reasons{border-left:4px solid var(--bad);background:#fef2f2;padding:8px 16px;margin:12px 0}.reasons ul{margin:4px 0;padding-left:20px}\
.card{border:1px solid var(--line);border-radius:6px;padding:10px 14px;margin:10px 0;break-inside:avoid}\
.card.blocking{border-left:4px solid var(--bad)}\
.tag{display:inline-block;font-size:11px;font-weight:700;text-transform:uppercase;border-radius:4px;padding:1px 7px;margin-right:6px;color:#fff;background:var(--mute)}\
.tag.critical,.tag.notmet,.tag.failed{background:var(--bad)}.tag.major,.tag.unclear,.tag.inconclusive{background:var(--warn)}\
.tag.minor{background:#2563eb}.tag.met,.tag.passed{background:var(--ok)}\
.loc,code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12px}\
pre{background:#f3f4f6;border:1px solid var(--line);border-radius:4px;padding:8px;margin:6px 0;white-space:pre-wrap;word-break:break-word}\
.label{color:var(--mute);font-weight:600}.rule{background:#eff6ff;border-left:3px solid #2563eb;padding:4px 10px;margin:6px 0}\
.muted{color:var(--mute)}footer{margin-top:32px;padding-top:10px;border-top:2px solid var(--line);color:var(--mute)}\
@media print{body{max-width:none;padding:0;font-size:12px}.verdict,.tag,.reasons,.rule,pre{-webkit-print-color-adjust:exact;print-color-adjust:exact}\
h2{break-after:avoid}a{color:inherit;text-decoration:none}}";

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn link(url: &str, label: &str) -> String {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        format!("<a href=\"{}\">{}</a>", escape(url), escape(label))
    } else {
        escape(label)
    }
}

fn tag(class: &str, text: &str) -> String {
    format!("<span class=\"tag {}\">{}</span>", escape(class), escape(text))
}

fn pre(text: &str) -> String {
    format!("<pre>{}</pre>", escape(text.trim_end_matches(['\n', '\r'])))
}

fn labelled(label: &str, text: &str) -> String {
    format!("<p><span class=\"label\">{}:</span> {}</p>", escape(label), escape(text))
}

pub fn render_html(doc: &HandoffDocument, input: &HandoffInput<'_>) -> String {
    let report = input.report;
    let mut out = String::new();
    let branch = doc.meta.branch.as_deref().unwrap_or("(detached)");
    let title = format!("qgate review: {} {} {} - {}", doc.meta.repo, branch, doc.meta.short_commit, doc.verdict.label);
    let _ = write!(
        out,
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{}</title><style>{STYLE}</style></head><body>\n",
        escape(&title)
    );
    header(&mut out, doc);
    criteria(&mut out, doc, report);
    findings(&mut out, &report.findings, &report.gate.blocking);
    checks(&mut out, &report.checks);
    if let Some(agents) = &report.agents {
        agents_section(&mut out, agents);
    }
    footer(&mut out, input);
    out.push_str("</body></html>\n");
    out
}

fn header(out: &mut String, doc: &HandoffDocument) {
    let meta = &doc.meta;
    let verdict = &doc.verdict;
    let _ = writeln!(out, "<header><h1>qgate review</h1>");
    let class = if verdict.passed { "pass" } else { "fail" };
    let _ = writeln!(out, "<div class=\"verdict {class}\">{}</div>", escape(verdict.label));
    if verdict.passed {
        let note = if verdict.incomplete {
            "The quality gate passed. Some files could not be reviewed."
        } else {
            "The quality gate passed."
        };
        let _ = writeln!(out, "<p>{note}</p>");
    } else {
        let _ = writeln!(out, "<div class=\"reasons\"><strong>Why it failed</strong><ul>");
        for reason in &verdict.reasons {
            let _ = writeln!(out, "<li>{}</li>", escape(reason));
        }
        let _ = writeln!(out, "</ul></div>");
    }
    out.push_str("<table class=\"meta\">\n");
    let mut row = |label: &str, value_html: String| {
        let _ = writeln!(out, "<tr><th>{}</th><td>{value_html}</td></tr>", escape(label));
    };
    row("Repository", escape(&meta.repo));
    row("Branch", escape(meta.branch.as_deref().unwrap_or("(detached)")));
    row("Commit", format!("<code>{}</code>", escape(&meta.commit)));
    let tickets = if doc.context.tickets.is_empty() {
        "none".to_string()
    } else {
        let items: Vec<String> = doc.context.tickets.iter().map(ticket_html).collect();
        items.join("<br>")
    };
    row("Ticket", tickets);
    row("Reviewed at", escape(&meta.reviewed_at));
    if let Some(url) = &meta.run_url {
        row("Dashboard", link(url, url));
    }
    out.push_str("</table></header>\n");
}

fn ticket_html(ticket: &super::document::TicketInfo) -> String {
    let key = ticket.url.as_deref().map_or_else(|| escape(&ticket.key), |url| link(url, &ticket.key));
    match &ticket.title {
        Some(title) => format!("{key} - {}", escape(title)),
        None => key,
    }
}

fn criteria(out: &mut String, doc: &HandoffDocument, report: &ReviewReport) {
    out.push_str("<section id=\"criteria\"><h2>Acceptance criteria</h2>\n");
    if doc.criteria.is_empty() {
        out.push_str("<p class=\"muted\">No acceptance criteria were assessed for this review.</p>\n");
    }
    let assessments = report.requirements.iter().flat_map(|r| r.assessments.iter());
    let evidence_of = |id: &str| -> &[EvidenceRef] {
        assessments.clone().find(|a| a.criterion_id == id).map_or(&[], |a| a.evidence.as_slice())
    };
    for row in &doc.criteria {
        let (class, label) = match row.status {
            CriterionStatus::Met => ("met", "Met"),
            CriterionStatus::NotMet => ("notmet", "Not met"),
            CriterionStatus::Unclear => ("unclear", "Unclear"),
        };
        let heading = format!("{} <strong>{}</strong> {}", tag(class, label), escape(&row.id), escape(&row.text));
        let _ = writeln!(out, "<div class=\"card\"><h3>{heading}</h3>");
        if !row.explanation.trim().is_empty() {
            let _ = writeln!(out, "<p>{}</p>", escape(&row.explanation));
        }
        let evidence = evidence_of(&row.id);
        if evidence.is_empty() {
            out.push_str("<p class=\"muted\">No code evidence.</p>\n");
        }
        for e in evidence {
            let _ = writeln!(
                out,
                "<div><span class=\"loc\">{}</span>{}</div>",
                escape(&location(&e.file, e.line)),
                pre(&e.quote)
            );
        }
        out.push_str("</div>\n");
    }
    out.push_str("</section>\n");
}

fn location(file: &str, line: u32) -> String {
    format!("{file}:{line}")
}

fn source_label(finding: &Finding) -> Option<String> {
    match &finding.source {
        FindingSource::Llm => None,
        FindingSource::StaticTool(name) if name.to_lowercase().contains("regression") => Some("regression".to_string()),
        FindingSource::StaticTool(name) => Some(format!("tool: {name}")),
    }
}

fn findings(out: &mut String, findings: &[Finding], blocking: &[usize]) {
    out.push_str("<section id=\"findings\"><h2>Findings</h2>\n");
    if findings.is_empty() {
        out.push_str("<p class=\"muted\">No findings.</p>\n");
    }
    for severity in Severity::ALL.into_iter().rev() {
        let group: Vec<(usize, &Finding)> =
            findings.iter().enumerate().filter(|(_, f)| f.severity == severity).collect();
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(out, "<h3>{} ({})</h3>", escape(&severity.as_str().to_uppercase()), group.len());
        for (index, finding) in group {
            finding_card(out, finding, blocking.contains(&index));
        }
    }
    out.push_str("</section>\n");
}

fn finding_card(out: &mut String, f: &Finding, is_blocking: bool) {
    let class = if is_blocking { "card blocking" } else { "card" };
    let mut tags = tag(f.severity.as_str(), f.severity.as_str()) + &tag("category", f.category.as_str());
    if let Some(source) = source_label(f) {
        tags.push_str(&tag("source", &source));
    }
    if is_blocking {
        tags.push_str(&tag("failed", "blocking"));
    }
    if f.demoted.is_some() {
        tags.push_str(&tag("unclear", "demoted"));
    }
    let _ = writeln!(out, "<div class=\"{class}\"><h3>{tags}{}</h3>", escape(&f.title));
    let _ = writeln!(out, "<p class=\"loc\">{}</p>", escape(&location(&f.file, f.line)));
    if !f.evidence.trim().is_empty() {
        out.push_str(&pre(&f.evidence));
    }
    out.push_str(&labelled("Explanation", &f.explanation));
    if let Some(demotion) = f.demoted {
        out.push_str(&labelled("Demoted", demotion.describe()));
    }
    if let Some(reproduce) = &f.reproduce {
        out.push_str("<div class=\"rule\"><span class=\"label\">Reproduce</span>\n");
        out.push_str(&labelled("Input", &reproduce.input));
        out.push_str(&labelled("Expected", &reproduce.expected));
        out.push_str(&labelled("Actual", &reproduce.actual));
        if let Some(command) = &reproduce.command {
            out.push_str(&pre(command));
        }
        out.push_str("</div>\n");
    }
    if let Some(suggestion) = f.suggestion.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&labelled("Suggestion", suggestion));
    }
    if let Some(rule) = &f.prevention_rule {
        let scope = rule.scope.as_deref().map(|s| format!(" (applies to {s})")).unwrap_or_default();
        let _ = writeln!(
            out,
            "<div class=\"rule\"><span class=\"label\">Prevention rule:</span> {}</div>",
            escape(&format!("{}{scope}", rule.text))
        );
    }
    out.push_str("</div>\n");
}

fn checks(out: &mut String, checks: &[CheckResult]) {
    out.push_str("<section id=\"checks\"><h2>Checks</h2>\n");
    if checks.is_empty() {
        out.push_str("<p class=\"muted\">No checks configured.</p>\n");
    }
    for check in checks {
        let (class, label) = match check.outcome {
            CheckOutcome::Passed => ("passed", "pass"),
            CheckOutcome::Failed => ("failed", "fail"),
            CheckOutcome::TimedOut => ("failed", "timed out"),
            CheckOutcome::Error => ("failed", "could not start"),
        };
        let required = if check.required { "required" } else { "optional" };
        let exit = check.exit_code.map_or_else(|| "none".to_string(), |c| c.to_string());
        let _ = writeln!(
            out,
            "<div class=\"card\"><h3>{} {}</h3><p class=\"muted\">{required}, exit code {exit}, {} ms</p>",
            tag(class, label),
            escape(&check.name),
            check.duration_ms
        );
        if !check.output_tail.trim().is_empty() {
            out.push_str(&pre(&check.output_tail));
        }
        out.push_str("</div>\n");
    }
    out.push_str("</section>\n");
}

fn agents_section(out: &mut String, agents: &AgentReport) {
    out.push_str("<section id=\"agents\"><h2>Agents</h2>\n");
    out.push_str("<h3>Generated tests</h3>\n");
    if agents.tests.is_empty() {
        out.push_str("<p class=\"muted\">No tests were generated.</p>\n");
    }
    for test in &agents.tests {
        test_card(out, test, agents);
    }
    out.push_str("<h3>Architecture rule violations</h3>\n");
    if agents.violations.is_empty() {
        out.push_str("<p class=\"muted\">None.</p>\n");
    }
    for violation in &agents.violations {
        violation_card(out, violation);
    }
    out.push_str("<h3>Design mismatches</h3>\n");
    if agents.design.is_empty() {
        out.push_str("<p class=\"muted\">None.</p>\n");
    }
    for mismatch in &agents.design {
        design_card(out, mismatch);
    }
    if !agents.notes.is_empty() {
        out.push_str("<h3>Notes</h3><ul>\n");
        for note in &agents.notes {
            let _ = writeln!(out, "<li>{}</li>", escape(note));
        }
        out.push_str("</ul>\n");
    }
    if let Some(cost) = agents.cost_usd {
        let _ = writeln!(out, "<p class=\"muted\">Agent cost: ${cost:.4}</p>");
    }
    out.push_str("</section>\n");
}

fn test_card(out: &mut String, test: &TestResult, agents: &AgentReport) {
    let (class, label) = match test.verdict {
        TestVerdict::Passed => ("passed", "passed"),
        TestVerdict::FailedAssertion => ("failed", "failed"),
        TestVerdict::Inconclusive => ("inconclusive", "inconclusive"),
    };
    let scenario = test
        .evidence
        .as_ref()
        .map(|e| &e.scenario)
        .or_else(|| agents.brief.iter().flat_map(|b| b.scenarios.iter()).find(|s| s.id == test.scenario_id));
    let title = scenario.map_or("", |s| s.title.as_str());
    let _ = writeln!(
        out,
        "<div class=\"card\"><h3>{} {} {}</h3>",
        tag(class, label),
        escape(&test.scenario_id),
        escape(title)
    );
    if let Some(s) = scenario {
        out.push_str(&labelled("Given", &s.given));
        out.push_str(&labelled("When", &s.when));
        out.push_str(&labelled("Then", &s.then));
        if !s.criterion_ids.is_empty() {
            out.push_str(&labelled("Proves", &s.criterion_ids.join(", ")));
        }
    }
    if !test.note.trim().is_empty() {
        out.push_str(&labelled("Note", &test.note));
    }
    if let Some(e) = &test.evidence {
        out.push_str(&labelled("Reproduce", &e.reproduce));
        if !e.output.trim().is_empty() {
            out.push_str(&pre(&e.output));
        }
        for code in &e.code_under_test {
            let _ = writeln!(
                out,
                "<div><span class=\"loc\">{}</span>{}</div>",
                escape(&location(&code.file, code.line)),
                pre(&code.quote)
            );
        }
    }
    out.push_str("</div>\n");
}

fn violation_card(out: &mut String, v: &RuleViolation) {
    let kind = if v.approved { tag("failed", "approved rule") } else { tag("inconclusive", "inferred rule") };
    let _ = writeln!(out, "<div class=\"card\"><h3>{kind}{} {}</h3>", escape(&v.rule_id), escape(&v.rule_title));
    let _ = writeln!(out, "<p class=\"loc\">{}</p>{}", escape(&location(&v.file, v.line)), pre(&v.evidence));
    out.push_str(&labelled("Explanation", &v.explanation));
    if let Some(suggestion) = v.suggestion.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&labelled("Suggestion", suggestion));
    }
    out.push_str("</div>\n");
}

fn design_card(out: &mut String, d: &DesignMismatch) {
    let _ = writeln!(
        out,
        "<div class=\"card\"><h3>{} {}</h3>",
        tag("major", &format!("{:?}", d.kind).to_lowercase()),
        escape(&d.frame)
    );
    out.push_str(&labelled("Design", &d.design_value));
    let _ = writeln!(out, "<p class=\"loc\">{}</p>{}", escape(&location(&d.file, d.line)), pre(&d.code_evidence));
    out.push_str(&labelled("Explanation", &d.explanation));
    out.push_str("</div>\n");
}

fn footer(out: &mut String, input: &HandoffInput<'_>) {
    let usage = &input.report.usage;
    let cost = input.report.estimated_cost_usd.map_or_else(|| "n/a".to_string(), |c| format!("${c:.4}"));
    let _ = writeln!(
        out,
        "<footer>Tokens: {} input, {} output, {} cache read, {} cache write. Estimated cost: {}. Generated by qgate.</footer>",
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
        escape(&cost)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff::domain::fixtures::{failing_report, meta, passing_report};
    use crate::handoff::domain::{BlockingRules, HandoffDocument};
    use crate::review::application::ReviewReport;

    fn render(report: &ReviewReport) -> String {
        let input = HandoffInput { meta: meta(), report, rules: BlockingRules::default() };
        render_html(&HandoffDocument::build(&input), &input)
    }

    #[test]
    fn finding_card_shows_reproduce_and_demotion_escaped() {
        use crate::review::application::{Demotion, Reproduce};
        let mut report = failing_report();
        report.findings[0].reproduce = Reproduce::new("name=<script>", "rejected", "stored as-is", "go test ./x");
        report.findings[0].demoted = Some(Demotion::MissingReproduce);
        let html = render(&report);
        assert!(html.contains("<span class=\"label\">Input:</span> name=&lt;script&gt;"));
        assert!(html.contains("<span class=\"label\">Actual:</span> stored as-is"));
        assert!(html.contains("<pre>go test ./x</pre>"));
        assert!(html.contains("tag unclear\">demoted"));
        assert!(!html.contains("<script>"));
    }

    fn position(html: &str, needle: &str) -> usize {
        html.find(needle).unwrap_or_else(|| panic!("missing {needle}"))
    }

    #[test]
    fn failing_report_renders_every_section_in_order() {
        let html = render(&failing_report());
        let order = [
            "<h1>qgate review</h1>",
            "verdict fail\">FAILED",
            "Acceptance criteria</h2>",
            "Findings</h2>",
            "Checks</h2>",
            "Agents</h2>",
            "<footer>",
        ];
        let positions: Vec<usize> = order.iter().map(|n| position(&html, n)).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("@media print"));
    }

    #[test]
    fn header_lists_blocking_reasons_and_metadata() {
        let html = render(&failing_report());
        for needle in [
            "Why it failed",
            "required checks failed: unit",
            "blocking findings: SQL injection (pay/db.go:42)",
            "acceptance criteria not met: AC-2",
            "shop-api",
            "feature/SHOP-7-pay",
            "3f9c2ab7d1e04c5b8a6f9e2d1c0b3a4f5e6d7c8b",
            "<a href=\"https://acme.atlassian.net/browse/SHOP-7\">SHOP-7</a> - Pay with tax",
            "2026-11-20 14:30:05 UTC",
            "<a href=\"http://localhost:8080/#run-42\">",
        ] {
            assert!(html.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn passing_report_shows_a_green_badge_without_reasons_and_handles_empty_sections() {
        let mut report = passing_report();
        report.gate.incomplete = true;
        let mut no_ticket = meta();
        no_ticket.ticket_keys.clear();
        let input = HandoffInput { meta: no_ticket, report: &report, rules: BlockingRules::default() };
        let html = render_html(&HandoffDocument::build(&input), &input);
        assert!(html.contains("verdict pass\">PASSED"));
        assert!(html.contains("The quality gate passed. Some files could not be reviewed."));
        assert!(!html.contains("Why it failed"));
        for empty in ["No acceptance criteria were assessed", "No findings.", "<td>none</td>"] {
            assert!(html.contains(empty), "missing {empty}");
        }
        assert!(!html.contains("Agents</h2>"));
        assert!(html.contains("Estimated cost: n/a"));
        assert!(!render(&passing_report()).contains("Some files could not"));
    }

    #[test]
    fn criteria_show_status_and_evidence_quotes() {
        let html = render(&failing_report());
        assert!(html.contains("tag met\">Met"));
        assert!(html.contains("tag notmet\">Not met"));
        assert!(html.contains("tag unclear\">Unclear"));
        assert!(html.contains("<strong>AC-1</strong> Total includes tax"));
        assert!(html.contains("<span class=\"loc\">pay/total.go:10</span><pre>total := net + tax</pre>"));
        assert!(html.contains("No code evidence."));
    }

    #[test]
    fn findings_are_grouped_by_severity_with_rules_and_sources() {
        let mut report = failing_report();
        let mut quality = report.findings[1].clone();
        quality.title = "Secret in repo".into();
        quality.source = FindingSource::StaticTool("gitleaks".into());
        let mut regression = report.findings[1].clone();
        regression.title = "Behaviour changed".into();
        regression.source = FindingSource::StaticTool("regression-suite".into());
        report.findings.extend([quality, regression]);
        let html = render(&report);
        let (critical, major, info) = (
            position(&html, "<h3>CRITICAL (1)</h3>"),
            position(&html, "<h3>MAJOR (3)</h3>"),
            position(&html, "<h3>INFO (1)</h3>"),
        );
        assert!(critical < major && major < info);
        assert!(!html.contains("MINOR"));
        assert!(html.contains("tag source\">tool: gitleaks"));
        assert!(html.contains("tag source\">regression"));
        assert!(html.contains("card blocking"));
        assert!(html.contains("tag failed\">blocking"));
        for needle in [
            "tag category\">security",
            "<span class=\"label\">Explanation:</span> SQL injection explained.",
            "<span class=\"label\">Suggestion:</span> Fix SQL injection.",
            "Prevention rule:</span> Build SQL with bound parameters only. (applies to **/*.go)",
            "<span class=\"label\">Prevention rule:</span> Name constants instead of magic numbers.</div>",
        ] {
            assert!(html.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn checks_show_outcome_exit_code_and_output_tail() {
        let html = render(&failing_report());
        assert!(html.contains("tag failed\">fail</span> unit"));
        assert!(html.contains("required, exit code 1, 1200 ms"));
        assert!(html.contains("<pre>unit: 1 failure</pre>"));
        assert!(html.contains("tag passed\">pass</span> build"));
        assert!(html.contains("optional, exit code 1"));
    }

    #[test]
    fn check_outcomes_cover_timeouts_and_missing_exit_codes() {
        let mut report = passing_report();
        report.checks[0].outcome = CheckOutcome::TimedOut;
        report.checks[0].exit_code = None;
        report.checks.push(CheckResult { outcome: CheckOutcome::Error, ..report.checks[0].clone() });
        let html = render(&report);
        assert!(html.contains("timed out") && html.contains("could not start") && html.contains("exit code none"));
    }

    #[test]
    fn agents_section_shows_tests_violations_design_notes_and_cost() {
        let mut report = failing_report();
        report.agents.as_mut().unwrap().cost_usd = Some(0.5);
        let html = render(&report);
        for needle in [
            "tag failed\">failed</span> S2 Total includes tax",
            "<span class=\"label\">Given:</span> a cart of 100",
            "<span class=\"label\">Then:</span> the total is 110",
            "<span class=\"label\">Proves:</span> SHOP-7/AC-1",
            "<span class=\"label\">Reproduce:</span> go test ./pay -run TestS2",
            "want 110, got 100",
            "tag inconclusive\">inconclusive</span> S1 Total includes tax",
            "<span class=\"label\">Note:</span> test did not compile",
            "tag failed\">approved rule</span>R1 R1 title",
            "tag inconclusive\">inferred rule</span>R2 R2 title",
            "<span class=\"label\">Suggestion:</span> Follow R1.",
            "tag major\">copy</span> Checkout / Pay",
            "<span class=\"label\">Design:</span> Pay now",
            "&lt;button&gt;Pay&lt;/button&gt;",
            "architecture: rulebook rebuilt",
            "Agent cost: $0.5000",
        ] {
            assert!(html.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn empty_and_passed_agent_results_are_labelled() {
        let mut report = passing_report();
        report.agents = Some(Default::default());
        let html = render(&report);
        assert!(html.contains("No tests were generated.") && html.matches("<p class=\"muted\">None.</p>").count() == 2);
        let mut agents = failing_report().agents.unwrap();
        agents.tests[0].verdict = TestVerdict::Passed;
        agents.tests[0].evidence = None;
        report.agents = Some(agents);
        assert!(render(&report).contains("tag passed\">passed</span> S2"));
    }

    #[test]
    fn footer_shows_usage_and_cost() {
        let mut report = failing_report();
        report.usage.input_tokens = 1234;
        report.usage.cache_read_tokens = 7;
        let html = render(&report);
        assert!(html.contains("Tokens: 1234 input, 0 output, 7 cache read, 0 cache write. Estimated cost: $0.1200."));
    }

    #[test]
    fn untrusted_text_is_escaped_everywhere() {
        let attack = "<script>alert(1)</script>";
        let mut report = failing_report();
        let finding = &mut report.findings[0];
        finding.title = attack.into();
        finding.evidence = attack.into();
        finding.explanation = attack.into();
        finding.suggestion = Some(attack.into());
        finding.file = attack.into();
        finding.source = FindingSource::StaticTool(attack.into());
        finding.prevention_rule =
            Some(crate::review::application::PreventionRule { text: attack.into(), scope: Some(attack.into()) });
        report.checks[0].name = attack.into();
        report.checks[0].output_tail = attack.into();
        report.gate.failed_checks = vec![attack.into()];
        let agents = report.agents.as_mut().unwrap();
        agents.notes = vec![attack.into()];
        agents.violations[0].evidence = attack.into();
        agents.design[0].frame = attack.into();
        agents.tests[0].note = attack.into();
        let requirements = report.requirements.as_mut().unwrap();
        requirements.requirements.criteria[0].text = attack.into();
        requirements.assessments[0].evidence[0].quote = attack.into();
        let mut input_meta = meta();
        input_meta.repo = attack.into();
        input_meta.branch = Some("\"><img src=x onerror=1>".into());
        let input = HandoffInput { meta: input_meta, report: &report, rules: BlockingRules::default() };
        let html = render_html(&HandoffDocument::build(&input), &input);
        assert!(!html.contains("<script>"), "raw script tag leaked");
        assert!(!html.contains("<img"), "raw tag leaked");
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn only_http_urls_become_links() {
        assert_eq!(link("javascript:alert(1)", "x<y"), "x&lt;y");
        assert_eq!(
            link(" https://a.test/?q=\"1\"&b ", "go"),
            "<a href=\"https://a.test/?q=&quot;1&quot;&amp;b\">go</a>"
        );
        let mut report = failing_report();
        report.requirements.as_mut().unwrap().requirements.url = Some("javascript:alert(1)".into());
        assert!(!render(&report).contains("href=\"javascript"));
    }

    #[test]
    fn output_is_self_contained() {
        let html = render(&failing_report());
        for forbidden in ["src=", "<link", "<script", "@import", "url(", "<iframe", "<img"] {
            assert!(!html.contains(forbidden), "external reference {forbidden}");
        }
        for (at, _) in html.match_indices("http") {
            assert!(
                html[..at].ends_with("href=\"") || html[..at].ends_with("\">"),
                "http reference outside an anchor at {at}"
            );
        }
    }

    #[test]
    fn escape_covers_html_special_characters() {
        assert_eq!(escape("<a href=\"x\">'&'</a>"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }
}
