//! Use case: the Ticket Analyst. Turns acceptance criteria, ticket description and design links
//! plus a compact view of the change into a validated, cached [`TicketBrief`].

use std::collections::HashSet;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::json;

use crate::review::application::agent_ports::{AgentError, AgentInput, TicketAnalyst};
use crate::review::application::{AcceptanceCriterion, TestScenario, TicketBrief};
use crate::review::domain::change_set::{ChangeStatus, DiffLine, FileChange};
use crate::tickets::domain::sha256_hex;

use super::ports::{BriefCache, BriefGenerator, BriefRequest};

/// Upper bound of the change view sent to the model.
pub const MAX_CHANGE_VIEW_CHARS: usize = 60_000;
/// Bump when the prompt or validation changes, so cached briefs are not reused.
const BRIEF_FORMAT_VERSION: &str = "ticket-brief-v1";
const SCENARIO_ID_PREFIX: &str = "S";

pub struct AnalyzeTicket {
    generator: Arc<dyn BriefGenerator>,
    cache: Arc<dyn BriefCache>,
    max_scenarios: usize,
}

impl AnalyzeTicket {
    /// `max_scenarios` is clamped to at least 1.
    pub fn new(generator: Arc<dyn BriefGenerator>, cache: Arc<dyn BriefCache>, max_scenarios: usize) -> Self {
        Self { generator, cache, max_scenarios: max_scenarios.max(1) }
    }

    pub async fn execute(&self, input: &AgentInput<'_>) -> Result<TicketBrief, AgentError> {
        let criteria = input.requirements.map(|r| r.criteria.clone()).unwrap_or_default();
        let (description, design_links) =
            input.ticket.map(|t| (t.description.clone(), t.design_links.clone())).unwrap_or_default();
        if criteria.is_empty() && description.trim().is_empty() {
            return Err(AgentError("no acceptance criteria or ticket description to analyse".into()));
        }
        let request = BriefRequest {
            criteria,
            description,
            design_links,
            change_view: render_change_view(input.files, MAX_CHANGE_VIEW_CHARS),
            max_scenarios: self.max_scenarios,
        };
        let hash = brief_hash(&request, input.files);
        match self.cache.get_brief(&hash).await {
            Ok(Some(brief)) => return Ok(brief),
            Ok(None) => {}
            Err(error) => eprintln!("qgate ticket analyst: {error}; generating a new brief"),
        }
        let raw = self.generator.generate(&request).await?;
        let brief = validate_brief(raw, &request.criteria, self.max_scenarios);
        if let Err(error) = self.cache.put_brief(&hash, &brief).await {
            eprintln!("qgate ticket analyst: brief not cached: {error}");
        }
        Ok(brief)
    }
}

impl TicketAnalyst for AnalyzeTicket {
    fn analyze<'a>(&'a self, input: &'a AgentInput<'a>) -> BoxFuture<'a, Result<TicketBrief, AgentError>> {
        Box::pin(self.execute(input))
    }
}

/// sha256 over criteria, description, design links, sorted changed paths and the full diff's hash.
fn brief_hash(request: &BriefRequest, files: &[FileChange]) -> String {
    let mut paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    paths.sort_unstable();
    let diff_hash = sha256_hex(render_change_view(files, usize::MAX).as_bytes());
    let material = json!({
        "version": BRIEF_FORMAT_VERSION,
        "criteria": request.criteria,
        "description": request.description,
        "design_links": request.design_links,
        "paths": paths,
        "diff": diff_hash,
        "max_scenarios": request.max_scenarios,
    });
    sha256_hex(material.to_string().as_bytes())
}

/// Keeps scenarios that only cite existing criteria and have a title and an expected outcome,
/// caps them at `max_scenarios` and renumbers them `S1..`.
pub fn validate_brief(raw: TicketBrief, criteria: &[AcceptanceCriterion], max_scenarios: usize) -> TicketBrief {
    let known: HashSet<&str> = criteria.iter().map(|c| c.id.as_str()).collect();
    let proposed = raw.scenarios.len();
    let valid: Vec<TestScenario> = raw
        .scenarios
        .into_iter()
        .filter_map(|scenario| {
            let ids = dedup_trimmed(scenario.criterion_ids);
            let is_valid = !ids.is_empty()
                && ids.iter().all(|id| known.contains(id.as_str()))
                && !scenario.title.trim().is_empty()
                && !scenario.then.trim().is_empty();
            is_valid.then_some(TestScenario { criterion_ids: ids, ..scenario })
        })
        .collect();
    let dropped = proposed - valid.len();
    let scenarios = valid
        .into_iter()
        .take(max_scenarios)
        .enumerate()
        .map(|(index, scenario)| TestScenario { id: format!("{SCENARIO_ID_PREFIX}{}", index + 1), ..scenario })
        .collect();
    let mut context = raw.context.trim().to_string();
    if dropped > 0 {
        let note = format!("(qgate dropped {dropped} proposed scenario(s) citing unknown or no acceptance criteria.)");
        context = if context.is_empty() { note } else { format!("{context} {note}") };
    }
    TicketBrief { scenarios, implicit_requirements: dedup_trimmed(raw.implicit_requirements), context }
}

fn dedup_trimmed(items: Vec<String>) -> Vec<String> {
    let mut unique: Vec<String> = Vec::with_capacity(items.len());
    for item in items.into_iter().map(|i| i.trim().to_string()).filter(|i| !i.is_empty()) {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    unique
}

/// File headers and diff hunks (`+`/`-`/` ` prefixed lines), cut at a line boundary after
/// `max_chars` bytes with a note of how many files were left out.
pub fn render_change_view(files: &[FileChange], max_chars: usize) -> String {
    let mut out = String::new();
    for (index, file) in files.iter().enumerate() {
        for line in file_lines(file) {
            if out.len() + line.len() + 1 > max_chars {
                let omitted = files.len() - index - 1;
                out.push_str(&format!("[change truncated in {}; {omitted} more file(s) omitted]\n", file.path));
                return out;
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn file_lines(file: &FileChange) -> Vec<String> {
    let status = match &file.status {
        ChangeStatus::Added => "added".to_string(),
        ChangeStatus::Modified => "modified".to_string(),
        ChangeStatus::Deleted => "deleted".to_string(),
        ChangeStatus::Renamed { from } => format!("renamed from {from}"),
    };
    let mut lines = vec![format!("=== {} ({status})", file.path)];
    if file.is_binary {
        lines.push("(binary file)".into());
        return lines;
    }
    for hunk in &file.hunks {
        lines.push(hunk.header.clone());
        lines.extend(hunk.lines.iter().map(|line| match line {
            DiffLine::Added { text, .. } => format!("+{text}"),
            DiffLine::Removed { text } => format!("-{text}"),
            DiffLine::Context { text, .. } => format!(" {text}"),
        }));
    }
    lines
}

#[cfg(test)]
#[path = "analyze_ticket_tests.rs"]
mod tests;
