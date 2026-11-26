use sha2::{Digest, Sha256};

use super::model::FindingFacts;

pub const FINGERPRINT_LEN: usize = 64;

pub fn normalize_evidence(evidence: &str) -> String {
    evidence
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn fingerprint(facts: &FindingFacts) -> String {
    let material =
        [facts.file.as_str(), &facts.category, &facts.source, &normalize_evidence(&facts.evidence)].join("\0");
    Sha256::digest(material.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn is_fingerprint(value: &str) -> bool {
    value.len() == FINGERPRINT_LEN && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod tests;
