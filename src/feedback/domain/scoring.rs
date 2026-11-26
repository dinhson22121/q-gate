use std::collections::BTreeMap;

use super::model::{KindScore, SeverityCap, Verdict};

pub const START: i32 = 100;
pub const STEP: i32 = 10;
pub const FLOOR: i32 = 10;
pub const MIN_JUDGMENTS: u32 = 10;
pub const MAJOR_AT: u8 = 50;
pub const MINOR_AT: u8 = 20;
const UNCAPPABLE_CATEGORY: &str = "security";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindEvent {
    pub kind: String,
    pub verdict: Verdict,
}

pub fn category_of(kind: &str) -> &str {
    kind.rsplit_once('/').map_or(kind, |(_, category)| category)
}

pub fn cap_for(kind: &str, score: u8, active: bool) -> Option<SeverityCap> {
    if !active || category_of(kind) == UNCAPPABLE_CATEGORY {
        return None;
    }
    if score <= MINOR_AT {
        Some(SeverityCap::Minor)
    } else if score <= MAJOR_AT {
        Some(SeverityCap::Major)
    } else {
        None
    }
}

pub fn replay(verdicts: impl IntoIterator<Item = Verdict>) -> (u8, u32) {
    let mut score = START;
    let mut count = 0;
    for verdict in verdicts {
        let delta = match verdict {
            Verdict::Accepted => STEP,
            Verdict::Dismissed => -STEP,
        };
        score = (score + delta).clamp(FLOOR, START);
        count += 1;
    }
    (score as u8, count)
}

pub fn score_kinds(events: &[KindEvent]) -> Vec<KindScore> {
    let mut grouped: BTreeMap<&str, Vec<Verdict>> = BTreeMap::new();
    for event in events {
        grouped.entry(&event.kind).or_default().push(event.verdict);
    }
    grouped
        .into_iter()
        .map(|(kind, verdicts)| {
            let (score, judgments) = replay(verdicts);
            let active = judgments >= MIN_JUDGMENTS;
            KindScore { kind: kind.to_string(), score, judgments, active, cap: cap_for(kind, score, active) }
        })
        .collect()
}

#[cfg(test)]
#[path = "scoring_tests.rs"]
mod tests;
