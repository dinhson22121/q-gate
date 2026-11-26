use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accepted,
    Dismissed,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Accepted => "accepted",
            Verdict::Dismissed => "dismissed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Verdict::Accepted),
            "dismissed" => Some(Verdict::Dismissed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityCap {
    Major,
    Minor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingFacts {
    pub file: String,
    pub category: String,
    pub source: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Assessment {
    pub verdict: Option<Verdict>,
    pub cap: Option<SeverityCap>,
    pub score: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindScore {
    pub kind: String,
    pub score: u8,
    pub judgments: u32,
    pub active: bool,
    pub cap: Option<SeverityCap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Judgment {
    pub fingerprint: String,
    pub verdict: Verdict,
}

pub fn kind_of(facts: &FindingFacts) -> String {
    format!("{}/{}", facts.source, facts.category)
}
