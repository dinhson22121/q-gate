use futures::future::BoxFuture;
use serde_json::Value;

use crate::policy::domain::{ExistingRule, Proposal, ProposalDraft, ProposalStatus, RuleApproval, RunFacts};
use crate::review::application::GatePolicy;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NeedsConfirmation(String),
    #[error("{0}")]
    ModelUnavailable(String),
    #[error("policy model failed: {0}")]
    Model(String),
    #[error("{0}")]
    Storage(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyPrompt {
    pub description: String,
    pub settings: Vec<(String, Value)>,
    pub locked: Vec<String>,
    pub repo: Option<String>,
    pub rules: Vec<ExistingRule>,
    pub repo_gate: Option<GatePolicy>,
    pub existing_checks: Vec<String>,
}

pub trait PolicyModel: Send + Sync {
    fn propose<'a>(&'a self, prompt: &'a PolicyPrompt) -> BoxFuture<'a, Result<ProposalDraft, PolicyError>>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsSnapshot {
    pub version: i64,
    pub config: Value,
    pub locked: Vec<String>,
    pub restart_required: Vec<String>,
    pub webhook_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsApplied {
    pub version: Option<i64>,
    pub changed_fields: Vec<String>,
    pub restart_required: Vec<String>,
}

pub trait SettingsGateway: Send + Sync {
    fn snapshot(&self) -> Result<SettingsSnapshot, PolicyError>;
    fn validate(&self, config: &Value) -> Result<(), String>;
    fn apply(
        &self,
        changes: &[(String, Value)],
        changed_by: &str,
        expected_version: i64,
    ) -> Result<SettingsApplied, PolicyError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleWrite {
    pub replaces: Option<(String, String)>,
    pub title: String,
    pub description: String,
    pub scope_globs: Vec<String>,
    pub status: RuleApproval,
}

pub trait RulebookGateway: Send + Sync {
    fn rules<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<ExistingRule>, PolicyError>>;
    fn repos(&self) -> BoxFuture<'_, Result<Vec<String>, PolicyError>>;
    fn write<'a>(&'a self, repo: &'a str, rules: Vec<RuleWrite>) -> BoxFuture<'a, Result<Vec<String>, PolicyError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingContext {
    pub repo_path: String,
    pub file: String,
    pub line: u32,
    pub severity: String,
    pub category: String,
    pub title: String,
    pub explanation: String,
    pub evidence: String,
}

pub trait RunHistory: Send + Sync {
    fn recent(&self, limit: u32) -> Result<(Vec<RunFacts>, usize), PolicyError>;
    fn finding(&self, run_id: i64, index: usize) -> Result<Option<FindingContext>, PolicyError>;
    fn repo_paths(&self) -> Result<Vec<String>, PolicyError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoGate {
    pub policy: GatePolicy,
    pub check_names: Vec<String>,
}

pub trait GatePolicySource: Send + Sync {
    fn repo_gate(&self, repo_path: &str) -> RepoGate;
    fn with_server(&self, policy: GatePolicy, settings: &Value) -> GatePolicy;
}

pub trait ProposalStore: Send + Sync {
    fn insert(&self, proposal: &Proposal) -> Result<i64, PolicyError>;
    fn get(&self, id: i64) -> Result<Option<Proposal>, PolicyError>;
    fn transition(&self, proposal: &Proposal, from: ProposalStatus) -> Result<bool, PolicyError>;
    fn recent(&self, limit: u32) -> Result<Vec<Proposal>, PolicyError>;
}
