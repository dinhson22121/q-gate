use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_NOTE_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionVerdict {
    VerifiedMet,
    NotApplicable,
    Clarified,
}

impl CriterionVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            CriterionVerdict::VerifiedMet => "verified_met",
            CriterionVerdict::NotApplicable => "not_applicable",
            CriterionVerdict::Clarified => "clarified",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "verified_met" => Some(CriterionVerdict::VerifiedMet),
            "not_applicable" => Some(CriterionVerdict::NotApplicable),
            "clarified" => Some(CriterionVerdict::Clarified),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriterionVerification {
    pub repo: String,
    pub criterion_id: String,
    pub text_hash: String,
    pub criterion_text: String,
    pub verdict: CriterionVerdict,
    pub note: String,
    pub recorded_at: DateTime<Utc>,
}

pub fn normalize_criterion_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn criterion_text_hash(text: &str) -> String {
    Sha256::digest(normalize_criterion_text(text).as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn validate_note(verdict: CriterionVerdict, note: &str) -> Result<String, String> {
    let note = note.trim();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(format!("note must be at most {MAX_NOTE_CHARS} characters"));
    }
    if verdict == CriterionVerdict::Clarified && note.is_empty() {
        return Err("a clarification needs a non-empty note".into());
    }
    Ok(note.to_string())
}

#[cfg(test)]
#[path = "criterion_tests.rs"]
mod tests;
