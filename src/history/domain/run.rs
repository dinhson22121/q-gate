use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::report_view::ReportView;
use crate::review::application::{CriterionStatus, ReviewReport, Severity};

/// What started a review run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Commit,
    Push,
    Manual,
    PrePush,
}

/// Outcome of a review run: gate passed, gate failed, or the run itself errored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} '{value}'")]
pub struct ParseError {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! string_enum {
    ($t:ty, $kind:literal, { $($variant:ident => $s:literal),* $(,)? }) => {
        impl $t {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),* }
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl FromStr for $t {
            type Err = ParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok(Self::$variant),)*
                    other => Err(ParseError { kind: $kind, value: other.to_string() }),
                }
            }
        }
    };
}

string_enum!(Trigger, "trigger", { Commit => "commit", Push => "push", Manual => "manual", PrePush => "pre_push" });
string_enum!(RunStatus, "status", { Passed => "passed", Failed => "failed", Error => "error" });

impl RunStatus {
    /// Status for a completed review: follows the gate decision.
    pub fn of_report(report: &ReviewReport) -> Self {
        if report.gate.passed { Self::Passed } else { Self::Failed }
    }
}

/// One review run. `R` is the report representation: the live [`ReviewReport`]
/// when recording, the deserialized [`ReportView`] when reading back.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewRun<R = ReviewReport> {
    /// 0 before the run is saved.
    pub id: i64,
    pub repo_path: String,
    pub repo_name: String,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub ticket_key: Option<String>,
    pub trigger: Trigger,
    pub created_at: DateTime<Utc>,
    pub status: RunStatus,
    pub error: Option<String>,
    pub report: Option<R>,
}

/// A run as read back from storage.
pub type StoredRun = ReviewRun<ReportView>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SeverityCounts {
    pub critical: u64,
    pub major: u64,
    pub minor: u64,
    pub info: u64,
}

impl SeverityCounts {
    pub fn add(&mut self, severity: Severity) {
        match severity {
            Severity::Critical => self.critical += 1,
            Severity::Major => self.major += 1,
            Severity::Minor => self.minor += 1,
            Severity::Info => self.info += 1,
        }
    }
}

/// Row of the runs table: run metadata plus counts, without the full report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunListItem {
    pub id: i64,
    pub repo_path: String,
    pub repo_name: String,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub ticket_key: Option<String>,
    pub trigger: Trigger,
    pub created_at: DateTime<Utc>,
    pub status: RunStatus,
    pub error: Option<String>,
    pub findings: SeverityCounts,
    pub criteria_met: u64,
    pub criteria_total: u64,
    pub estimated_cost_usd: Option<f64>,
}

impl From<&StoredRun> for RunListItem {
    fn from(run: &StoredRun) -> Self {
        let mut findings = SeverityCounts::default();
        let (mut criteria_met, mut criteria_total) = (0, 0);
        if let Some(report) = &run.report {
            report.findings.iter().for_each(|f| findings.add(f.severity));
            if let Some(req) = &report.requirements {
                criteria_total = req.assessments.len() as u64;
                criteria_met = req.assessments.iter().filter(|a| a.status == CriterionStatus::Met).count() as u64;
            }
        }
        Self {
            id: run.id,
            repo_path: run.repo_path.clone(),
            repo_name: run.repo_name.clone(),
            commit: run.commit.clone(),
            branch: run.branch.clone(),
            ticket_key: run.ticket_key.clone(),
            trigger: run.trigger,
            created_at: run.created_at,
            status: run.status,
            error: run.error.clone(),
            findings,
            criteria_met,
            criteria_total,
            estimated_cost_usd: run.report.as_ref().and_then(|r| r.estimated_cost_usd),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_strings() {
        for t in [Trigger::Commit, Trigger::Push, Trigger::Manual, Trigger::PrePush] {
            assert_eq!(t.as_str().parse::<Trigger>().unwrap(), t);
        }
        for s in [RunStatus::Passed, RunStatus::Failed, RunStatus::Error] {
            assert_eq!(s.to_string().parse::<RunStatus>().unwrap(), s);
        }
        let err = "bogus".parse::<RunStatus>().unwrap_err();
        assert_eq!(err.to_string(), "unknown status 'bogus'");
    }
}
