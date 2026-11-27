use std::sync::Arc;

use serde_json::json;

use super::ports::{CriteriaCache, CriteriaInferrer};
use crate::review::application::AcceptanceCriterion;
use crate::tickets::domain::sha256_hex;

pub const MAX_INFERRED_CRITERIA: usize = 8;
const INFER_FORMAT_VERSION: &str = "inferred-criteria-v1";
const INFERRED_ID_INFIX: &str = "/AC-I";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InferredCriteria {
    pub criteria: Vec<AcceptanceCriterion>,
    pub note: Option<String>,
}

pub struct InferCriteria {
    inferrer: Arc<dyn CriteriaInferrer>,
    cache: Arc<dyn CriteriaCache>,
}

impl InferCriteria {
    pub fn new(inferrer: Arc<dyn CriteriaInferrer>, cache: Arc<dyn CriteriaCache>) -> Self {
        Self { inferrer, cache }
    }

    pub async fn execute(&self, key: &str, title: &str, description: &str) -> InferredCriteria {
        if description.trim().is_empty() {
            return InferredCriteria::default();
        }
        let hash = sha256_hex(
            json!({ "v": INFER_FORMAT_VERSION, "title": title, "description": description }).to_string().as_bytes(),
        );
        let texts = match self.cached_or_inferred(&hash, title, description).await {
            Ok(texts) => texts,
            Err(error) => {
                return InferredCriteria { criteria: Vec::new(), note: Some(format!("{key}: {error}")) };
            }
        };
        let criteria: Vec<AcceptanceCriterion> = texts
            .into_iter()
            .enumerate()
            .map(|(index, text)| AcceptanceCriterion::inferred(format!("{key}{INFERRED_ID_INFIX}{}", index + 1), text))
            .collect();
        let note = (!criteria.is_empty()).then(|| {
            format!(
                "{key} has no written acceptance criteria; {} were inferred from its description (advisory, not blocking)",
                criteria.len()
            )
        });
        InferredCriteria { criteria, note }
    }

    async fn cached_or_inferred(&self, hash: &str, title: &str, description: &str) -> Result<Vec<String>, String> {
        match self.cache.get_criteria(hash).await {
            Ok(Some(texts)) => return Ok(texts),
            Ok(None) => {}
            Err(error) => eprintln!("qgate criteria inference: {error}; inferring again"),
        }
        let raw = self.inferrer.infer(title, description).await.map_err(|e| format!("criteria not inferred: {e}"))?;
        let texts = clean(raw);
        if let Err(error) = self.cache.put_criteria(hash, &texts).await {
            eprintln!("qgate criteria inference: not cached: {error}");
        }
        Ok(texts)
    }
}

fn clean(raw: Vec<String>) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for text in raw.into_iter().map(|t| t.split_whitespace().collect::<Vec<_>>().join(" ")) {
        if !text.is_empty() && !unique.contains(&text) {
            unique.push(text);
        }
    }
    unique.truncate(MAX_INFERRED_CRITERIA);
    unique
}

#[cfg(test)]
#[path = "infer_criteria_tests.rs"]
mod tests;
