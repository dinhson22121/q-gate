//! Markdown rendering of a [`HandoffDocument`], for a coding agent and a human reader alike.

use std::fmt::Write as _;

use super::document::{CriterionRow, HandoffDocument, Task};
use super::rules_markdown::candidate_line;
use super::text::{cell, code_span, fenced, one_line, prose};
use crate::review::application::CriterionStatus;

const BLOCKING_PREFIX: &str = "";
const WARNING_PREFIX: &str = "W";

pub fn render_markdown(doc: &HandoffDocument) -> String {
    let mut out = String::new();
    header(&mut out, doc);
    tasks(&mut out, "Do this first (blocking)", BLOCKING_PREFIX, &doc.blocking, "Nothing blocking.");
    tasks(&mut out, "Should fix (warnings)", WARNING_PREFIX, &doc.warnings, "No warnings.");
    criteria(&mut out, &doc.criteria);
    rules_to_add(&mut out, doc);
    context(&mut out, doc);
    agent_instructions(&mut out, doc);
    out
}

fn header(out: &mut String, doc: &HandoffDocument) {
    let meta = &doc.meta;
    let branch = meta.branch.as_deref().unwrap_or("(detached)");
    let _ = writeln!(
        out,
        "# qgate review: {} {} {} - {}\n",
        one_line(&meta.repo),
        one_line(branch),
        meta.short_commit,
        doc.verdict.label
    );
    let _ = writeln!(
        out,
        "**Verdict: {}** - {} blocking, {} warnings.\n",
        doc.verdict.label,
        doc.blocking.len(),
        doc.warnings.len()
    );
    if doc.verdict.passed {
        out.push_str("The quality gate passed.");
        if doc.verdict.incomplete {
            out.push_str(" Some files could not be reviewed (see Context).");
        }
        out.push_str("\n\n");
    } else {
        out.push_str("Why it failed:\n\n");
        for reason in &doc.verdict.reasons {
            let _ = writeln!(out, "- {}", one_line(reason));
        }
        out.push('\n');
    }
    let _ = writeln!(out, "- Repository: {}", code_span(&meta.repo));
    let _ = writeln!(out, "- Branch: {}", code_span(branch));
    let _ = writeln!(out, "- Commit: {}", code_span(&meta.commit));
    if !doc.context.tickets.is_empty() {
        let keys: Vec<&str> = doc.context.tickets.iter().map(|t| t.key.as_str()).collect();
        let _ = writeln!(out, "- Tickets: {}", one_line(&keys.join(", ")));
    }
    let _ = writeln!(out, "- Reviewed at: {}", meta.reviewed_at);
    if let Some(url) = &meta.run_url {
        let _ = writeln!(out, "- Dashboard: <{}>", one_line(url));
    }
    out.push('\n');
}

fn tasks(out: &mut String, heading: &str, prefix: &str, list: &[Task], empty: &str) {
    let _ = writeln!(out, "## {heading}\n");
    if list.is_empty() {
        let _ = writeln!(out, "{empty}\n");
        return;
    }
    for (index, item) in list.iter().enumerate() {
        task(out, &format!("{prefix}{}", index + 1), item);
    }
}

fn task(out: &mut String, number: &str, item: &Task) {
    let _ = writeln!(out, "### {number}. {}\n", one_line(&item.title));
    if let Some(location) = &item.location {
        let _ = writeln!(out, "- Where: {}", code_span(location));
    }
    for fact in &item.facts {
        let _ = writeln!(out, "- {}: {}", fact.label, one_line(&fact.value));
    }
    out.push('\n');
    if !item.problem.trim().is_empty() {
        let _ = writeln!(out, "What's wrong:\n\n{}\n", prose(&item.problem));
    }
    for block in &item.code {
        let _ = writeln!(out, "{}:\n\n{}", one_line(&block.label), fenced(&block.language, &block.content));
    }
    if let Some(expected) = &item.expected {
        let _ = writeln!(out, "Expected:\n\n{}\n", prose(expected));
    }
    if let Some(fix) = &item.fix {
        let _ = writeln!(out, "Suggested fix:\n\n{}\n", prose(fix));
    }
}

fn status_label(status: CriterionStatus) -> &'static str {
    match status {
        CriterionStatus::Met => "met",
        CriterionStatus::NotMet => "NOT MET",
        CriterionStatus::Unclear => "unclear",
    }
}

fn criteria(out: &mut String, rows: &[CriterionRow]) {
    out.push_str("## Acceptance criteria status\n\n");
    if rows.is_empty() {
        out.push_str("No acceptance criteria were checked for this commit.\n\n");
        return;
    }
    out.push_str("| ID | Criterion | Status | Evidence |\n|---|---|---|---|\n");
    for row in rows {
        let evidence = if row.evidence.is_empty() { "-".to_string() } else { row.evidence.join(", ") };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&row.id),
            cell(&row.text),
            status_label(row.status),
            cell(&evidence)
        );
    }
    out.push('\n');
}

fn rules_to_add(out: &mut String, doc: &HandoffDocument) {
    if doc.rules_to_add.is_empty() {
        return;
    }
    out.push_str("## Rules to add\n\n");
    match &doc.rules_file {
        Some(path) => {
            let _ = writeln!(
                out,
                "New rules from this review, already added to {} for your coding agent:\n",
                code_span(path)
            );
        }
        None => out.push_str("Rules from this review; add them to your coding agent's instructions:\n\n"),
    }
    for rule in &doc.rules_to_add {
        let _ = writeln!(out, "- {}", candidate_line(rule));
    }
    out.push('\n');
}

fn context(out: &mut String, doc: &HandoffDocument) {
    let context = &doc.context;
    out.push_str("## Context\n\n");
    if context.tickets.is_empty() {
        out.push_str("- Ticket: none linked to this commit\n");
    }
    for ticket in &context.tickets {
        let mut line = format!("- Ticket {}", one_line(&ticket.key));
        if let Some(title) = ticket.title.as_deref().filter(|t| !t.trim().is_empty()) {
            let _ = write!(line, ": {}", one_line(title));
        }
        if let Some(url) = &ticket.url {
            let _ = write!(line, " (<{}>)", one_line(url));
        }
        let _ = writeln!(out, "{line}");
    }
    if let Some(description) = &context.description {
        let _ = writeln!(out, "- Ticket description (summary): {}", one_line(description));
    }
    list(out, "Implicit requirements (suggested, not required)", &context.implicit_requirements);
    let links: Vec<String> = context.design_links.iter().map(|l| format!("<{}>", one_line(l))).collect();
    list(out, "Figma links", &links);
    list(out, "Agent notes", &context.notes);
    list(out, "Files not reviewed", &context.skipped_files);
    out.push('\n');
}

fn list(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(out, "- {title}:");
    for item in items {
        let _ = writeln!(out, "  - {}", one_line(item));
    }
}

fn agent_instructions(out: &mut String, doc: &HandoffDocument) {
    let commit_hint = match doc.primary_ticket() {
        Some(key) => format!("reference the ticket key in the commit message (e.g. \"{} ...\")", one_line(key)),
        None => "reference the ticket key in the branch name or commit message".to_string(),
    };
    out.push_str("## For coding agents\n\n");
    out.push_str("1. Fix the items under \"Do this first\" in order; each one is self-contained.\n");
    out.push_str(
        "2. Do not weaken, skip or delete the generated tests: they encode the acceptance criteria. They are not \
         in your working tree; to reproduce, save the test source at the path shown and run the reproduce \
         command. Change the production code, not the assertions.\n",
    );
    out.push_str("3. Keep changes minimal and focused on the listed items; then handle the warnings.\n");
    out.push_str("4. Re-run the reproduce commands and the failing checks until they pass.\n");
    let _ = writeln!(out, "5. Commit and {commit_hint}, so qgate reviews the new commit.");
}
