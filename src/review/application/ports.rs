//! Ports the review use case depends on. Infrastructure adapters implement them.

use std::future::Future;

use crate::review::domain::change_set::{ChangeSet, FileChange};
use crate::review::domain::checks::{CheckResult, CheckSpec};
use crate::review::domain::finding::Finding;
use crate::review::domain::report::TokenUsage;
use crate::review::domain::requirements::{CriterionAssessment, Requirements};

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("git failed: {0}")]
    Git(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Produces the set of changes to review (local working tree, branch diff, PR...).
pub trait ChangeSource {
    fn load(&self) -> impl Future<Output = Result<ChangeSet, SourceError>> + Send;
}

pub struct ReviewInput<'a> {
    pub files: &'a [FileChange],
    /// Repository-specific review guidelines (may be empty).
    pub guidelines: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReviewerOutput {
    /// Unverified findings as reported by the reviewer.
    pub findings: Vec<Finding>,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviewerError {
    #[error("response was truncated (output token limit reached)")]
    Truncated,
    #[error("the model declined to review this batch (category: {})", category.as_deref().unwrap_or("unspecified"))]
    Refused { category: Option<String> },
    #[error("invalid response from model: {0}")]
    InvalidResponse(String),
    #[error("API temporarily unavailable after retries: {0}")]
    Unavailable(String),
    /// Authentication, permission or malformed request: retrying or continuing is pointless.
    #[error("API request rejected: {0}")]
    Rejected(String),
}

impl ReviewerError {
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
}

pub struct RequirementsInput<'a> {
    /// Every reviewable file of the change (the assessment needs the whole picture, not a batch).
    pub files: &'a [FileChange],
    pub requirements: &'a Requirements,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssessmentOutput {
    /// Unverified assessments as reported by the reviewer.
    pub assessments: Vec<CriterionAssessment>,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
}

/// Semantic code reviewer (an LLM in practice).
pub trait CodeReviewer {
    /// Finds defects in a batch of changed files.
    fn review(&self, input: ReviewInput<'_>) -> impl Future<Output = Result<ReviewerOutput, ReviewerError>> + Send;

    /// Judges, per acceptance criterion, whether the change implements it.
    fn assess_requirements(
        &self,
        input: RequirementsInput<'_>,
    ) -> impl Future<Output = Result<AssessmentOutput, ReviewerError>> + Send;
}

/// Runs the repository's deterministic checks (tests, linters, builds) in the repo root.
pub trait CheckRunner {
    fn run(&self, checks: &[CheckSpec]) -> impl Future<Output = Vec<CheckResult>> + Send;
}
