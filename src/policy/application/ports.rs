//! Ports of the policy context. Settings, rulebook and history are other contexts: their adapters call
//! those contexts' application layers. Blocking ports are called on the blocking pool.

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
    /// Stale, expired or already decided proposals; changes refused by the settings or rulebook.
    #[error("{0}")]
    Conflict(String),
    /// A sensitive change was selected without the separate confirmation.
    #[error("{0}")]
    NeedsConfirmation(String),
    /// No API key or model configured.
    #[error("{0}")]
    ModelUnavailable(String),
    #[error("policy model failed: {0}")]
    Model(String),
    #[error("{0}")]
    Storage(String),
}

/// What the model sees. All of it is data; `description` is untrusted user text.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyPrompt {
    pub description: String,
    /// Settable paths with their current values (webhook masked).
    pub settings: Vec<(String, Value)>,
    pub locked: Vec<String>,
    pub repo: Option<String>,
    pub rules: Vec<ExistingRule>,
    pub repo_gate: Option<GatePolicy>,
    pub existing_checks: Vec<String>,
}

/// Turns a description into a structured draft. Never applies anything.
pub trait PolicyModel: Send + Sync {
    fn propose<'a>(&'a self, prompt: &'a PolicyPrompt) -> BoxFuture<'a, Result<ProposalDraft, PolicyError>>;
}

/// The server settings in effect.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsSnapshot {
    /// Latest stored settings version (0 before the first save).
    pub version: i64,
    /// Effective config as JSON, `notify.webhook_url` masked as "".
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

/// Settings context, through its update/validation use cases (blocking).
pub trait SettingsGateway: Send + Sync {
    fn snapshot(&self) -> Result<SettingsSnapshot, PolicyError>;
    /// Whether `config` (a full effective config as JSON) passes the settings validation; the message names the field.
    fn validate(&self, config: &Value) -> Result<(), String>;
    /// Applies `changes` (path, new value) as one new settings version, refused when the latest version is
    /// no longer `expected_version`.
    fn apply(
        &self,
        changes: &[(String, Value)],
        changed_by: &str,
        expected_version: i64,
    ) -> Result<SettingsApplied, PolicyError>;
}

/// A rule to write into a repository's rulebook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleWrite {
    /// (id, title) of the rule this one replaces.
    pub replaces: Option<(String, String)>,
    pub title: String,
    pub description: String,
    pub scope_globs: Vec<String>,
    pub status: RuleApproval,
}

/// Conventions context (rulebooks).
pub trait RulebookGateway: Send + Sync {
    fn rules<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<ExistingRule>, PolicyError>>;
    fn repos(&self) -> BoxFuture<'_, Result<Vec<String>, PolicyError>>;
    /// Writes the rules; returns their titles.
    fn write<'a>(&'a self, repo: &'a str, rules: Vec<RuleWrite>) -> BoxFuture<'a, Result<Vec<String>, PolicyError>>;
}

/// A stored finding, for the "prevent this" entry point.
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

/// History context (blocking).
pub trait RunHistory: Send + Sync {
    /// Gate facts of the newest `limit` runs with a report, plus how many recent runs had none.
    fn recent(&self, limit: u32) -> Result<(Vec<RunFacts>, usize), PolicyError>;
    fn finding(&self, run_id: i64, index: usize) -> Result<Option<FindingContext>, PolicyError>;
    /// Repository paths of recent runs.
    fn repo_paths(&self) -> Result<Vec<String>, PolicyError>;
}

/// A repository's `.qgate.toml` as the gate sees it (blocking; repositories are read-only).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoGate {
    /// Repository part of the gate policy (agent policies at their defaults).
    pub policy: GatePolicy,
    pub check_names: Vec<String>,
}

pub trait GatePolicySource: Send + Sync {
    fn repo_gate(&self, repo_path: &str) -> RepoGate;
    /// `policy` with the server's agent policies from `settings` (effective config JSON) applied.
    fn with_server(&self, policy: GatePolicy, settings: &Value) -> GatePolicy;
}

/// Proposal persistence (blocking).
pub trait ProposalStore: Send + Sync {
    /// Stores a new proposal and returns its id.
    fn insert(&self, proposal: &Proposal) -> Result<i64, PolicyError>;
    fn get(&self, id: i64) -> Result<Option<Proposal>, PolicyError>;
    /// Saves the proposal's status and outcome only if its stored status is still `from`; returns whether it did.
    fn transition(&self, proposal: &Proposal, from: ProposalStatus) -> Result<bool, PolicyError>;
    /// The newest proposals, newest first.
    fn recent(&self, limit: u32) -> Result<Vec<Proposal>, PolicyError>;
}
