//! Ports of the conventions context. The store and repository reader are blocking (called on the
//! blocking pool); the model is async and dyn-compatible.

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
    /// The request is based on an outdated rulebook or cannot be applied to it.
    #[error("{0}")]
    Conflict(String),
}

/// What the model sees when deriving a rulebook. All text is untrusted repository content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRequest {
    pub sources: Vec<SourceFile>,
    pub samples: Vec<SourceFile>,
    pub layout: Vec<String>,
    /// (language, file count), most common first.
    pub languages: Vec<(String, usize)>,
}

/// What the model sees when checking a change.
#[derive(Debug, Clone, Copy)]
pub struct CheckRequest<'a> {
    /// Every rule the review may use, in rulebook order. Stable per rulebook (cached prompt prefix).
    pub rules: &'a [Rule],
    /// Ids of the rules whose scope matches this change.
    pub selected: &'a [&'a str],
    pub files: &'a [ChangedFile],
    /// Ticket context from the analyst, if any.
    pub context: Option<&'a str>,
}

/// A violation as reported by the model, before grounding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViolationDraft {
    pub rule_id: String,
    pub file: String,
    pub line: u32,
    pub evidence: String,
    pub explanation: String,
    pub suggestion: Option<String>,
}

/// The LLM behind the conventions context.
pub trait RuleModel: Send + Sync {
    fn draft_rules<'a>(&'a self, request: &'a DraftRequest) -> BoxFuture<'a, Result<Vec<RuleDraft>, ConventionsError>>;

    fn find_violations<'a>(
        &'a self,
        request: CheckRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<ViolationDraft>, ConventionsError>>;
}

/// Read-only access to a repository's files (blocking).
pub trait RepoReader: Send + Sync {
    /// Repository-relative, `/`-separated paths of the files at HEAD.
    fn list_files(&self, root: &Path) -> Result<Vec<String>, ConventionsError>;

    /// Up to `cap` bytes of a text file; `None` when missing, binary or not a regular file.
    fn read_capped(&self, root: &Path, path: &str, cap: usize) -> Result<Option<String>, ConventionsError>;
}

/// A human decision on a rule, keyed by its normalized title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDecision {
    pub normalized_title: String,
    pub status: RuleStatus,
    pub decided_at: DateTime<Utc>,
}

/// Rulebook persistence (blocking).
pub trait RulebookStore: Send + Sync {
    fn load(&self, repo: &str) -> Result<Option<Rulebook>, ConventionsError>;
    /// Inserts or replaces the repository's rulebook.
    fn save(&self, rulebook: &Rulebook) -> Result<(), ConventionsError>;
    /// Saves the rulebook and records the decision atomically.
    fn save_with_decision(&self, rulebook: &Rulebook, decision: &RuleDecision) -> Result<(), ConventionsError> {
        self.save_with_decisions(rulebook, std::slice::from_ref(decision))
    }
    /// Saves the rulebook and records every decision atomically.
    fn save_with_decisions(&self, rulebook: &Rulebook, decisions: &[RuleDecision]) -> Result<(), ConventionsError>;
    /// normalized title → status, for every decision ever made on the repository.
    fn decisions(&self, repo: &str) -> Result<HashMap<String, RuleStatus>, ConventionsError>;
    /// Repositories with a stored rulebook, sorted.
    fn repos(&self) -> Result<Vec<String>, ConventionsError>;
}
