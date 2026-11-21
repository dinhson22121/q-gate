//! Ticket view: which runs concern a Jira ticket, and how each of its acceptance criteria
//! progressed over those runs. Pure.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::report_view::ReportView;
use super::run::{RunListItem, StoredRun};
use crate::review::application::CriterionStatus;

const MAX_PROJECT_LEN: usize = 10;
const MAX_NUMBER_LEN: usize = 12;

/// A validated Jira issue key: `^[A-Z][A-Z0-9]{1,9}-[1-9][0-9]{0,11}$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TicketKey(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid ticket key (expected e.g. SHOP-7)")]
pub struct InvalidTicketKey;

impl TicketKey {
    pub fn parse(raw: &str) -> Result<Self, InvalidTicketKey> {
        let (project, number) = raw.split_once('-').ok_or(InvalidTicketKey)?;
        let project_ok = (2..=MAX_PROJECT_LEN).contains(&project.len())
            && project.starts_with(|c: char| c.is_ascii_uppercase())
            && project.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let number_ok = (1..=MAX_NUMBER_LEN).contains(&number.len())
            && !number.starts_with('0')
            && number.chars().all(|c| c.is_ascii_digit());
        if project_ok && number_ok { Ok(Self(raw.to_string())) } else { Err(InvalidTicketKey) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when a criterion id belongs to this ticket. Ids are `KEY/AC-n`; legacy ids without a
    /// `/` belong to the report's requirements key (`owner`).
    fn owns_criterion(&self, criterion_id: &str, owner: &str) -> bool {
        match criterion_id.split_once('/') {
            Some((prefix, _)) => prefix == self.0,
            None => owner == self.0,
        }
    }
}

impl fmt::Display for TicketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// True when the run concerns `key`: its ticket, the requirements key, the ticket context keys
/// (Sub-task + parent Story), or a criterion id prefixed with the key.
pub fn mentions_ticket(run: &StoredRun, key: &TicketKey) -> bool {
    let key_str = key.as_str();
    if run.ticket_key.as_deref() == Some(key_str) {
        return true;
    }
    let Some(report) = &run.report else { return false };
    report.ticket.as_ref().is_some_and(|t| t.keys.iter().any(|k| k == key_str))
        || report.requirements.as_ref().is_some_and(|r| {
            r.requirements.key == key_str
                || r.requirements.criteria.iter().any(|c| key.owns_criterion(&c.id, &r.requirements.key))
                || r.assessments.iter().any(|a| key.owns_criterion(&a.criterion_id, &r.requirements.key))
        })
}

/// One assessment of a criterion in one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriterionPoint {
    pub run_id: i64,
    pub commit: Option<String>,
    pub created_at: DateTime<Utc>,
    pub status: CriterionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriterionProgress {
    pub id: String,
    /// Text from the newest run that listed the criterion.
    pub text: String,
    /// Status in the newest run that assessed the criterion.
    pub latest: CriterionPoint,
    /// Every assessment, newest first.
    pub history: Vec<CriterionPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TicketProgress {
    pub key: String,
    /// Runs concerning the ticket, newest first.
    pub runs: Vec<RunListItem>,
    /// The ticket's criteria in first-seen order (newest run first).
    pub criteria: Vec<CriterionProgress>,
}

/// Builds the ticket view from runs that concern `key` (any order; they are sorted newest first).
pub fn ticket_progress(key: &TicketKey, runs: &[StoredRun]) -> TicketProgress {
    let mut ordered: Vec<&StoredRun> = runs.iter().collect();
    ordered.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));

    let mut criteria: Vec<CriterionProgress> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for run in &ordered {
        let Some(report) = &run.report else { continue };
        for (id, text, status) in ticket_assessments(key, report) {
            let point =
                CriterionPoint { run_id: run.id, commit: run.commit.clone(), created_at: run.created_at, status };
            match index.get(id) {
                Some(&i) => {
                    let entry = &mut criteria[i];
                    if entry.text.is_empty() {
                        entry.text = text.to_string();
                    }
                    entry.history.push(point);
                }
                None => {
                    index.insert(id.to_string(), criteria.len());
                    criteria.push(CriterionProgress {
                        id: id.to_string(),
                        text: text.to_string(),
                        latest: point.clone(),
                        history: vec![point],
                    });
                }
            }
        }
    }
    TicketProgress {
        key: key.as_str().to_string(),
        runs: ordered.into_iter().map(RunListItem::from).collect(),
        criteria,
    }
}

/// `(criterion id, text, status)` for each assessment in `report` that belongs to `key`.
fn ticket_assessments<'a>(key: &TicketKey, report: &'a ReportView) -> Vec<(&'a str, &'a str, CriterionStatus)> {
    let Some(req) = &report.requirements else { return Vec::new() };
    let owner = req.requirements.key.as_str();
    req.assessments
        .iter()
        .filter(|a| key.owns_criterion(&a.criterion_id, owner))
        .map(|a| {
            let text =
                req.requirements.criteria.iter().find(|c| c.id == a.criterion_id).map_or("", |c| c.text.as_str());
            (a.criterion_id.as_str(), text, a.status)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::domain::fixtures::{at, report_view, stored, with_criteria};
    use crate::history::domain::{RunStatus, TicketContextView};

    fn key(raw: &str) -> TicketKey {
        TicketKey::parse(raw).unwrap()
    }

    #[test]
    fn parses_only_well_formed_keys() {
        for ok in ["SHOP-7", "AB-1", "A1B2C3D4E5-123456789012", "QG-10"] {
            assert_eq!(TicketKey::parse(ok).unwrap().as_str(), ok, "{ok}");
        }
        for bad in [
            "",
            "shop-7",
            "S-7",
            "SHOP-0",
            "SHOP-07",
            "SHOP7",
            "SHOP-",
            "-7",
            "1SHOP-7",
            "ABCDEFGHIJK-1",
            "SHOP-1234567890123",
            "SHOP-7/AC-1",
            "SHOP-7 ",
            "SHÖP-7",
            "SH_P-7",
            "SHOP-7-1",
            "SHOP-%",
        ] {
            assert_eq!(TicketKey::parse(bad), Err(InvalidTicketKey), "{bad:?}");
        }
    }

    #[test]
    fn mentions_ticket_matches_every_place_a_key_can_appear() {
        let base = || report_view(Vec::new(), &[], 0);
        let by_column =
            StoredRun { ticket_key: Some("SHOP-7".into()), ..stored(1, "api", at(1, 0), RunStatus::Passed, None) };
        let by_context = stored(
            2,
            "api",
            at(1, 0),
            RunStatus::Passed,
            Some(ReportView {
                ticket: Some(TicketContextView { keys: vec!["SHOP-9".into(), "SHOP-7".into()], ..Default::default() }),
                ..base()
            }),
        );
        let by_criterion = stored(
            3,
            "api",
            at(1, 0),
            RunStatus::Passed,
            Some(with_criteria("SHOP-9", &[("SHOP-7/AC-1", CriterionStatus::Met)])),
        );
        let by_requirements = stored(4, "api", at(1, 0), RunStatus::Passed, Some(with_criteria("SHOP-7", &[])));
        let other = stored(
            5,
            "api",
            at(1, 0),
            RunStatus::Passed,
            Some(with_criteria("SHOP-70", &[("SHOP-70/AC-1", CriterionStatus::Met), ("AC-2", CriterionStatus::Met)])),
        );
        let k = key("SHOP-7");
        for run in [&by_column, &by_context, &by_criterion, &by_requirements] {
            assert!(mentions_ticket(run, &k), "run {}", run.id);
        }
        assert!(!mentions_ticket(&other, &k));
        assert!(!mentions_ticket(&stored(6, "api", at(1, 0), RunStatus::Error, None), &k));
    }

    #[test]
    fn progress_tracks_latest_status_per_criterion_newest_first() {
        let old = stored(
            1,
            "api",
            at(1, 0),
            RunStatus::Failed,
            Some(with_criteria(
                "SHOP-8",
                &[("SHOP-7/AC-1", CriterionStatus::NotMet), ("SHOP-7/AC-2", CriterionStatus::Unclear)],
            )),
        );
        let new = stored(
            2,
            "api",
            at(3, 0),
            RunStatus::Passed,
            Some(with_criteria(
                "SHOP-8",
                &[("SHOP-7/AC-1", CriterionStatus::Met), ("SHOP-8/AC-1", CriterionStatus::NotMet)],
            )),
        );
        let legacy = stored(
            3,
            "api",
            at(2, 0),
            RunStatus::Failed,
            Some(with_criteria("SHOP-7", &[("AC-3", CriterionStatus::Met)])),
        );

        let progress = ticket_progress(&key("SHOP-7"), &[old, new, legacy]);

        assert_eq!(progress.key, "SHOP-7");
        assert_eq!(progress.runs.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 3, 1]);
        let ids: Vec<&str> = progress.criteria.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["SHOP-7/AC-1", "AC-3", "SHOP-7/AC-2"]);
        let ac1 = &progress.criteria[0];
        assert_eq!(ac1.text, "text of SHOP-7/AC-1");
        assert_eq!((ac1.latest.run_id, ac1.latest.status), (2, CriterionStatus::Met));
        assert_eq!(
            ac1.history.iter().map(|p| (p.run_id, p.status)).collect::<Vec<_>>(),
            vec![(2, CriterionStatus::Met), (1, CriterionStatus::NotMet)]
        );
        assert_eq!(progress.criteria[2].latest.status, CriterionStatus::Unclear);
        assert_eq!(progress.criteria[2].latest.commit.as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn progress_of_runs_without_reports_lists_runs_only() {
        let run = StoredRun { ticket_key: Some("SHOP-7".into()), ..stored(1, "api", at(1, 0), RunStatus::Error, None) };
        let progress = ticket_progress(&key("SHOP-7"), &[run]);
        assert_eq!(progress.runs.len(), 1);
        assert!(progress.criteria.is_empty());
    }
}
