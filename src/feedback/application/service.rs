use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::ports::{FeedbackError, FeedbackStore};
use crate::feedback::domain::fingerprint::{fingerprint, is_fingerprint};
use crate::feedback::domain::model::{Assessment, FindingFacts, Judgment, KindScore, Scope, Verdict, kind_of};
use crate::feedback::domain::scoring::score_kinds;

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct FeedbackService {
    store: Arc<dyn FeedbackStore>,
    clock: Clock,
}

pub(super) fn require(name: &str, value: &str) -> Result<(), FeedbackError> {
    if value.trim().is_empty() {
        return Err(FeedbackError::InvalidInput(format!("{name} must not be empty")));
    }
    Ok(())
}

impl FeedbackService {
    pub fn new(store: Arc<dyn FeedbackStore>) -> Self {
        Self { store, clock: Arc::new(Utc::now) }
    }

    pub fn with_clock(self, clock: Clock) -> Self {
        Self { clock, ..self }
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    pub fn record(
        &self,
        repo: &str,
        facts: &FindingFacts,
        verdict: Verdict,
        scope: Scope,
    ) -> Result<String, FeedbackError> {
        require("repo", repo)?;
        require("source", &facts.source)?;
        require("category", &facts.category)?;
        let print = fingerprint(facts);
        self.store.put(repo, &print, &kind_of(facts), verdict, scope, self.now())?;
        Ok(print)
    }

    pub fn remove(&self, repo: &str, fingerprint: &str) -> Result<bool, FeedbackError> {
        require("repo", repo)?;
        if !is_fingerprint(fingerprint) {
            return Err(FeedbackError::InvalidInput("fingerprint must be 64 lowercase hex characters".into()));
        }
        self.store.delete(repo, fingerprint)
    }

    pub fn assess(&self, repo: &str, findings: &[FindingFacts]) -> Result<Vec<Assessment>, FeedbackError> {
        require("repo", repo)?;
        if findings.is_empty() {
            return Ok(Vec::new());
        }
        let prints: Vec<String> = findings.iter().map(fingerprint).collect();
        let now = self.now();
        let verdicts: HashMap<String, Verdict> = self
            .store
            .find(repo, &prints)?
            .into_iter()
            .filter_map(|j| j.effective_verdict(now).map(|verdict| (j.fingerprint, verdict)))
            .collect();
        let scores: HashMap<String, KindScore> = self.scores(repo)?.into_iter().map(|s| (s.kind.clone(), s)).collect();
        Ok(findings
            .iter()
            .zip(&prints)
            .map(|(facts, print)| {
                let score = scores.get(&kind_of(facts));
                let verdict = verdicts.get(print).copied();
                Assessment {
                    verdict,
                    cap: score.and_then(|s| s.cap).filter(|_| verdict != Some(Verdict::Accepted)),
                    score: score.map(|s| s.score),
                }
            })
            .collect())
    }

    pub fn judgments(&self, repo: &str, fingerprints: &[String]) -> Result<Vec<Judgment>, FeedbackError> {
        require("repo", repo)?;
        if fingerprints.is_empty() {
            return Ok(Vec::new());
        }
        self.store.find(repo, fingerprints)
    }

    pub fn scores(&self, repo: &str) -> Result<Vec<KindScore>, FeedbackError> {
        require("repo", repo)?;
        Ok(score_kinds(&self.store.events(repo)?))
    }

    pub fn reset(&self, repo: &str, kind: &str) -> Result<(), FeedbackError> {
        require("repo", repo)?;
        require("kind", kind)?;
        self.store.reset(repo, kind)
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
