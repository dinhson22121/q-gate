use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub const DISMISSAL_TTL_DAYS: i64 = 90;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Repo,
    Global,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Repo => "repo",
            Scope::Global => "global",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "repo" => Some(Scope::Repo),
            "global" => Some(Scope::Global),
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
    pub repo: String,
    pub fingerprint: String,
    pub verdict: Verdict,
    pub scope: Scope,
    pub recorded_at: DateTime<Utc>,
}

impl Judgment {
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        (self.verdict == Verdict::Dismissed).then(|| self.recorded_at + Duration::days(DISMISSAL_TTL_DAYS))
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at().is_some_and(|at| now >= at)
    }

    pub fn effective_verdict(&self, now: DateTime<Utc>) -> Option<Verdict> {
        (!self.is_expired(now)).then_some(self.verdict)
    }
}

pub fn kind_of(facts: &FindingFacts) -> String {
    format!("{}/{}", facts.source, facts.category)
}
