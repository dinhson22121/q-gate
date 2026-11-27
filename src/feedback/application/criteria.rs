use std::collections::HashMap;
use std::sync::Arc;

use super::ports::{CriterionStore, FeedbackError};
use super::service::{Clock, require};
use crate::feedback::domain::criterion::{CriterionVerdict, CriterionVerification, criterion_text_hash, validate_note};

pub struct CriterionVerifications {
    store: Arc<dyn CriterionStore>,
    clock: Clock,
}

impl CriterionVerifications {
    pub fn new(store: Arc<dyn CriterionStore>) -> Self {
        Self { store, clock: Arc::new(chrono::Utc::now) }
    }

    pub fn with_clock(self, clock: Clock) -> Self {
        Self { clock, ..self }
    }

    pub fn verify(
        &self,
        repo: &str,
        criterion_id: &str,
        criterion_text: &str,
        verdict: CriterionVerdict,
        note: &str,
    ) -> Result<CriterionVerification, FeedbackError> {
        require("repo", repo)?;
        require("criterion_id", criterion_id)?;
        require("criterion_text", criterion_text)?;
        let note = validate_note(verdict, note).map_err(FeedbackError::InvalidInput)?;
        let verification = CriterionVerification {
            repo: repo.to_string(),
            criterion_id: criterion_id.to_string(),
            text_hash: criterion_text_hash(criterion_text),
            criterion_text: criterion_text.trim().to_string(),
            verdict,
            note,
            recorded_at: (self.clock)(),
        };
        self.store.put_criterion(&verification)?;
        Ok(verification)
    }

    pub fn revoke(&self, repo: &str, criterion_id: &str, criterion_text: &str) -> Result<bool, FeedbackError> {
        require("repo", repo)?;
        require("criterion_id", criterion_id)?;
        require("criterion_text", criterion_text)?;
        self.store.delete_criterion(repo, criterion_id, &criterion_text_hash(criterion_text))
    }

    pub fn list(&self, repo: &str) -> Result<Vec<CriterionVerification>, FeedbackError> {
        require("repo", repo)?;
        self.store.list_criteria(repo)
    }

    pub fn lookup(
        &self,
        repo: &str,
        criteria: &[(String, String)],
    ) -> Result<Vec<Option<CriterionVerification>>, FeedbackError> {
        require("repo", repo)?;
        if criteria.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<(String, String)> =
            criteria.iter().map(|(id, text)| (id.clone(), criterion_text_hash(text))).collect();
        let found: HashMap<(String, String), CriterionVerification> = self
            .store
            .find_criteria(repo, &keys)?
            .into_iter()
            .map(|v| ((v.criterion_id.clone(), v.text_hash.clone()), v))
            .collect();
        Ok(keys.iter().map(|key| found.get(key).cloned()).collect())
    }
}

#[cfg(test)]
#[path = "criteria_tests.rs"]
mod tests;
