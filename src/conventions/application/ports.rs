use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;

use crate::conventions::domain::evidence::ChangedFile;
use crate::conventions::domain::rulebook::{Rule, RuleDraft, RuleStatus, Rulebook};
use crate::conventions::domain::sources::SourceFile;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConventionsError {
    #[error("rule model failed: {0}")]
    Model(String),
    #[error("rulebook storage failed: {0}")]
    Storage(String),
    #[error("cannot read repository: {0}")]
    Repository(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRequest {
    pub sources: Vec<SourceFile>,
    pub samples: Vec<SourceFile>,
    pub layout: Vec<String>,
    pub languages: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Copy)]
pub struct CheckRequest<'a> {
    pub rules: &'a [Rule],
    pub selected: &'a [&'a str],
    pub files: &'a [ChangedFile],
    pub context: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViolationDraft {
    pub rule_id: String,
    pub file: String,
    pub line: u32,
    pub evidence: String,
    pub explanation: String,
    pub suggestion: Option<String>,
}

pub trait RuleModel: Send + Sync {
    fn draft_rules<'a>(&'a self, request: &'a DraftRequest) -> BoxFuture<'a, Result<Vec<RuleDraft>, ConventionsError>>;

    fn find_violations<'a>(
        &'a self,
        request: CheckRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<ViolationDraft>, ConventionsError>>;
}

pub trait RepoReader: Send + Sync {
    fn list_files(&self, root: &Path) -> Result<Vec<String>, ConventionsError>;

    fn read_capped(&self, root: &Path, path: &str, cap: usize) -> Result<Option<String>, ConventionsError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDecision {
    pub normalized_title: String,
    pub status: RuleStatus,
    pub decided_at: DateTime<Utc>,
}

pub trait RulebookStore: Send + Sync {
    fn load(&self, repo: &str) -> Result<Option<Rulebook>, ConventionsError>;
    fn save(&self, rulebook: &Rulebook) -> Result<(), ConventionsError>;
    fn save_with_decision(&self, rulebook: &Rulebook, decision: &RuleDecision) -> Result<(), ConventionsError> {
        self.save_with_decisions(rulebook, std::slice::from_ref(decision))
    }
    fn save_with_decisions(&self, rulebook: &Rulebook, decisions: &[RuleDecision]) -> Result<(), ConventionsError>;
    fn decisions(&self, repo: &str) -> Result<HashMap<String, RuleStatus>, ConventionsError>;
    fn repos(&self) -> Result<Vec<String>, ConventionsError>;
}
