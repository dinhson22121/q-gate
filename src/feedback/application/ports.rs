use chrono::{DateTime, Utc};

use crate::feedback::domain::criterion::CriterionVerification;
use crate::feedback::domain::model::{Judgment, Scope, Verdict};
use crate::feedback::domain::scoring::KindEvent;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeedbackError {
    #[error("feedback storage failed: {0}")]
    Storage(String),
    #[error("invalid feedback input: {0}")]
    InvalidInput(String),
}

pub trait FeedbackStore: Send + Sync {
    fn put(
        &self,
        repo: &str,
        fingerprint: &str,
        kind: &str,
        verdict: Verdict,
        scope: Scope,
        recorded_at: DateTime<Utc>,
    ) -> Result<(), FeedbackError>;
    fn delete(&self, repo: &str, fingerprint: &str) -> Result<bool, FeedbackError>;
    fn find(&self, repo: &str, fingerprints: &[String]) -> Result<Vec<Judgment>, FeedbackError>;
    fn events(&self, repo: &str) -> Result<Vec<KindEvent>, FeedbackError>;
    fn reset(&self, repo: &str, kind: &str) -> Result<(), FeedbackError>;
}

pub trait CriterionStore: Send + Sync {
    fn put_criterion(&self, verification: &CriterionVerification) -> Result<(), FeedbackError>;
    fn delete_criterion(&self, repo: &str, criterion_id: &str, text_hash: &str) -> Result<bool, FeedbackError>;
    fn list_criteria(&self, repo: &str) -> Result<Vec<CriterionVerification>, FeedbackError>;
    fn find_criteria(&self, repo: &str, keys: &[(String, String)])
    -> Result<Vec<CriterionVerification>, FeedbackError>;
}
