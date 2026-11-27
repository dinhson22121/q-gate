use chrono::{DateTime, Utc};
use serde::Serialize;

use super::rules::{RuleCandidate, candidates};
use super::tasks;
use crate::review::application::{CriterionStatus, GatePolicy, HumanMark, ReviewReport, Unverifiable};

const SHORT_SHA_LEN: usize = 7;
const DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S UTC";
const DESCRIPTION_SUMMARY_CHARS: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMeta {
    pub repo: String,
    pub branch: Option<String>,
    pub commit: String,
    pub ticket_keys: Vec<String>,
    pub reviewed_at: DateTime<Utc>,
    pub run_id: Option<i64>,
    pub dashboard_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingRules {
    pub unmet_criteria: bool,
    pub failed_tests: bool,
    pub rule_violations: bool,
    pub incomplete: bool,
}

impl From<&GatePolicy> for BlockingRules {
    fn from(policy: &GatePolicy) -> Self {
        Self {
            unmet_criteria: policy.fail_on_unmet_criteria,
            failed_tests: policy.fail_on_test_failure,
            rule_violations: policy.fail_on_rule_violation,
            incomplete: policy.fail_on_incomplete,
        }
    }
}

impl Default for BlockingRules {
    fn default() -> Self {
        Self::from(&GatePolicy::default())
    }
}

#[derive(Debug, Clone)]
pub struct HandoffInput<'a> {
    pub meta: RunMeta,
    pub report: &'a ReviewReport,
    pub rules: BlockingRules,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentMeta {
    pub repo: String,
    pub branch: Option<String>,
    pub commit: String,
    pub short_commit: String,
    pub ticket_keys: Vec<String>,
    pub reviewed_at: String,
    pub run_id: Option<i64>,
    pub run_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub passed: bool,
    pub label: &'static str,
    pub incomplete: bool,
    pub reasons: Vec<String>,
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    FailedCheck,
    Finding,
    FailedTest,
    InconclusiveTest,
    UnmetCriterion,
    UnclearCriterion,
    RuleViolation,
    DesignMismatch,
    OptionalCheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeBlock {
    pub label: String,
    pub language: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    pub kind: TaskKind,
    pub title: String,
    pub location: Option<String>,
    pub problem: String,
    pub facts: Vec<Fact>,
    pub code: Vec<CodeBlock>,
    pub expected: Option<String>,
    pub fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriterionRow {
    pub id: String,
    pub text: String,
    pub inferred: bool,
    pub status: CriterionStatus,
    pub explanation: String,
    pub evidence: Vec<String>,
    pub human: Option<HumanMark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TicketInfo {
    pub key: String,
    pub title: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HandoffContext {
    pub tickets: Vec<TicketInfo>,
    pub implicit_requirements: Vec<String>,
    pub design_links: Vec<String>,
    pub description: Option<String>,
    pub notes: Vec<String>,
    pub skipped_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffDocument {
    pub meta: DocumentMeta,
    pub verdict: Verdict,
    pub blocking: Vec<Task>,
    pub warnings: Vec<Task>,
    pub criteria: Vec<CriterionRow>,
    pub context: HandoffContext,
    pub rules_to_add: Vec<RuleCandidate>,
    pub rules_file: Option<String>,
    pub unverifiable: Vec<Unverifiable>,
}

impl HandoffDocument {
    pub fn build(input: &HandoffInput<'_>) -> Self {
        let (blocking, warnings) = tasks::collect(input);
        Self {
            meta: document_meta(&input.meta),
            verdict: verdict(input),
            blocking,
            warnings,
            criteria: criteria(input.report),
            context: context(input),
            rules_to_add: candidates(input.report),
            rules_file: None,
            unverifiable: input.report.unverifiable.clone(),
        }
    }

    pub fn primary_ticket(&self) -> Option<&str> {
        self.context.tickets.first().map(|t| t.key.as_str())
    }
}

fn document_meta(meta: &RunMeta) -> DocumentMeta {
    let run_url = match (&meta.dashboard_url, meta.run_id) {
        (Some(base), Some(id)) => Some(format!("{}/#run-{id}", base.trim_end_matches('/'))),
        _ => None,
    };
    DocumentMeta {
        repo: meta.repo.clone(),
        branch: meta.branch.clone(),
        commit: meta.commit.clone(),
        short_commit: meta.commit.chars().take(SHORT_SHA_LEN).collect(),
        ticket_keys: meta.ticket_keys.clone(),
        reviewed_at: meta.reviewed_at.format(DATE_FORMAT).to_string(),
        run_id: meta.run_id,
        run_url,
    }
}

fn verdict(input: &HandoffInput<'_>) -> Verdict {
    let gate = &input.report.gate;
    let rules = input.rules;
    let mut reasons = Vec::new();
    let mut add = |active: bool, items: &[String], what: &str| {
        if active && !items.is_empty() {
            reasons.push(format!("{what}: {}", items.join(", ")));
        }
    };
    add(true, &gate.failed_checks, "required checks failed");
    let blocking_findings: Vec<String> = gate
        .blocking
        .iter()
        .filter_map(|&i| input.report.findings.get(i))
        .map(|f| format!("{} ({}:{})", f.title.trim(), f.file, f.line))
        .collect();
    add(true, &blocking_findings, "blocking findings");
    add(rules.failed_tests, &gate.failed_tests, "generated tests failed");
    add(rules.unmet_criteria, &gate.unmet_criteria, "acceptance criteria not met");
    add(rules.rule_violations, &gate.rule_violations, "approved architecture rules violated");
    add(true, &gate.design_mismatches, "design mismatches");
    if gate.incomplete && rules.incomplete {
        reasons.push("review incomplete: some files could not be reviewed".into());
    }
    if !gate.passed && reasons.is_empty() {
        reasons.push("the gate failed (see the dashboard for details)".into());
    }
    if gate.passed {
        reasons.clear();
    }
    Verdict {
        passed: gate.passed,
        label: if gate.passed { "PASSED" } else { "FAILED" },
        incomplete: gate.incomplete,
        reasons,
        unverified: gate.unverified_criteria.clone(),
    }
}

fn criteria(report: &ReviewReport) -> Vec<CriterionRow> {
    let Some(requirements) = &report.requirements else { return Vec::new() };
    requirements
        .assessments
        .iter()
        .map(|a| CriterionRow {
            id: a.criterion_id.clone(),
            text: tasks::criterion_text(requirements, &a.criterion_id),
            inferred: tasks::criterion_inferred(requirements, &a.criterion_id),
            status: a.status,
            explanation: a.explanation.clone(),
            evidence: a.evidence.iter().map(|e| format!("{}:{}", e.file, e.line)).collect(),
            human: a.human,
        })
        .collect()
}

fn context(input: &HandoffInput<'_>) -> HandoffContext {
    let mut tickets: Vec<TicketInfo> = Vec::new();
    let mut push_key = |key: &str, title: Option<String>, url: Option<String>| {
        if !key.trim().is_empty() && !tickets.iter().any(|t| t.key == key) {
            tickets.push(TicketInfo { key: key.to_string(), title, url });
        }
    };
    if let Some(r) = &input.report.requirements {
        push_key(&r.requirements.key, Some(r.requirements.title.clone()), r.requirements.url.clone());
    }
    let ticket = input.report.ticket.as_ref();
    ticket.iter().flat_map(|t| t.keys.iter()).for_each(|k| push_key(k, None, None));
    input.meta.ticket_keys.iter().for_each(|k| push_key(k, None, None));
    let agents = input.report.agents.as_ref();
    HandoffContext {
        tickets,
        implicit_requirements: agents
            .and_then(|a| a.brief.as_ref())
            .map(|b| b.implicit_requirements.clone())
            .unwrap_or_default(),
        design_links: ticket.map(|t| t.design_links.clone()).unwrap_or_default(),
        description: ticket.and_then(|t| summary(&t.description)),
        notes: agents
            .map(|a| a.notes.clone())
            .unwrap_or_default()
            .into_iter()
            .chain(input.report.notes.clone())
            .collect(),
        skipped_files: input.report.skipped.iter().map(|s| format!("{} ({})", s.path, s.reason.describe())).collect(),
    }
}

fn summary(description: &str) -> Option<String> {
    let line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    let mut cut: String = line.chars().take(DESCRIPTION_SUMMARY_CHARS).collect();
    if cut.len() < line.len() {
        cut.push_str(" ...");
    }
    Some(cut)
}
