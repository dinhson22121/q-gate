use std::future::Future;

use futures::future::BoxFuture;

use crate::review::domain::change_set::{ChangeSet, FileChange};
use crate::review::domain::checks::{CheckResult, CheckSpec};
use crate::review::domain::finding::Finding;
use crate::review::domain::report::TokenUsage;
use crate::review::domain::requirements::{CriterionAssessment, Requirements};
use crate::review::domain::verification::FindingAudit;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("git failed: {0}")]
    Git(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub trait ChangeSource {
    fn load(&self) -> impl Future<Output = Result<ChangeSet, SourceError>> + Send;
}

pub struct ReviewInput<'a> {
    pub files: &'a [FileChange],
    pub guidelines: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReviewerOutput {
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
    #[error("API request rejected: {0}")]
    Rejected(String),
    #[error("{0}")]
    RateLimited(String),
}

impl ReviewerError {
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Rejected(_) | Self::RateLimited(_))
    }
}

pub struct RequirementsInput<'a> {
    pub files: &'a [FileChange],
    pub requirements: &'a Requirements,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssessmentOutput {
    pub assessments: Vec<CriterionAssessment>,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
}

pub trait CodeReviewer {
    fn review(&self, input: ReviewInput<'_>) -> impl Future<Output = Result<ReviewerOutput, ReviewerError>> + Send;

    fn assess_requirements(
        &self,
        input: RequirementsInput<'_>,
    ) -> impl Future<Output = Result<AssessmentOutput, ReviewerError>> + Send;
}

pub trait CheckRunner {
    fn run(&self, checks: &[CheckSpec]) -> impl Future<Output = Vec<CheckResult>> + Send;
}

pub struct AuditInput<'a> {
    pub files: &'a [FileChange],
    pub findings: &'a [Finding],
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditOutput {
    pub audits: Vec<FindingAudit>,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
}

pub trait FindingAuditor: Send + Sync {
    fn audit<'a>(&'a self, input: AuditInput<'a>) -> BoxFuture<'a, Result<AuditOutput, ReviewerError>>;
}
