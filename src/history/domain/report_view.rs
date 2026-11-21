use serde::{Deserialize, Serialize};

use super::agent_view::{AgentReportView, TicketContextView};
use crate::review::application::{Category, Confidence, CriterionStatus, Severity};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportView {
    #[serde(default)]
    pub files_reviewed: Vec<String>,
    #[serde(default)]
    pub skipped: Vec<SkippedView>,
    #[serde(default)]
    pub findings: Vec<FindingView>,
    #[serde(default)]
    pub rejected: Vec<RejectedView>,
    #[serde(default)]
    pub requirements: Option<RequirementsView>,
    #[serde(default)]
    pub checks: Vec<CheckView>,
    pub gate: GateView,
    #[serde(default)]
    pub usage: UsageView,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub agents: Option<AgentReportView>,
    #[serde(default)]
    pub ticket: Option<TicketContextView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedView {
    pub path: String,
    pub reason: SkipReasonView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipReasonView {
    pub kind: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingView {
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub category: Category,
    pub confidence: Confidence,
    pub title: String,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub prevention_rule: Option<PreventionRuleView>,
    #[serde(default)]
    pub source: Option<FindingSourceView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreventionRuleView {
    pub text: String,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingSourceView {
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedView {
    pub finding: FindingView,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementsView {
    pub requirements: TicketView,
    #[serde(default)]
    pub assessments: Vec<AssessmentView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketView {
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub criteria: Vec<CriterionView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionView {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentView {
    pub criterion_id: String,
    pub status: CriterionStatus,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub evidence: Vec<EvidenceView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceView {
    pub file: String,
    pub line: u32,
    #[serde(default)]
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckView {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    pub outcome: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub output_tail: String,
}

impl CheckView {
    pub fn is_failed(&self) -> bool {
        self.outcome != "passed"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateView {
    pub passed: bool,
    #[serde(default)]
    pub blocking: Vec<usize>,
    #[serde(default)]
    pub unmet_criteria: Vec<String>,
    #[serde(default)]
    pub failed_checks: Vec<String>,
    #[serde(default)]
    pub incomplete: bool,
    #[serde(default)]
    pub failed_tests: Vec<String>,
    #[serde(default)]
    pub rule_violations: Vec<String>,
    #[serde(default)]
    pub design_mismatches: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageView {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}
