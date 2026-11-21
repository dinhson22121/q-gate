use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::run::{RunStatus, SeverityCounts, StoredRun};
use crate::review::application::{Category, CriterionStatus};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CriteriaCounts {
    pub met: u64,
    pub not_met: u64,
    pub unclear: u64,
}

/// Aggregates over the runs created at or after `since`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodStats {
    pub since: DateTime<Utc>,
    pub runs: u64,
    pub passed: u64,
    pub failed: u64,
    pub errored: u64,
    /// passed / (passed + failed); errored runs never reached the gate. `None` with no gated runs.
    pub pass_rate: Option<f64>,
    pub findings_by_severity: SeverityCounts,
    /// Every category is present (zero when unseen), keyed by its snake_case name.
    pub findings_by_category: BTreeMap<&'static str, u64>,
    pub criteria: CriteriaCounts,
    /// Number of check executions that did not pass.
    pub failed_checks: u64,
    pub estimated_cost_usd: f64,
}

pub fn period_stats(runs: &[StoredRun], since: DateTime<Utc>) -> PeriodStats {
    let mut stats = PeriodStats {
        since,
        runs: 0,
        passed: 0,
        failed: 0,
        errored: 0,
        pass_rate: None,
        findings_by_severity: SeverityCounts::default(),
        findings_by_category: Category::ALL.iter().map(|c| (c.as_str(), 0)).collect(),
        criteria: CriteriaCounts::default(),
        failed_checks: 0,
        estimated_cost_usd: 0.0,
    };
    for run in runs.iter().filter(|r| r.created_at >= since) {
        stats.runs += 1;
        match run.status {
            RunStatus::Passed => stats.passed += 1,
            RunStatus::Failed => stats.failed += 1,
            RunStatus::Error => stats.errored += 1,
        }
        let Some(report) = &run.report else { continue };
        for finding in &report.findings {
            stats.findings_by_severity.add(finding.severity);
            *stats.findings_by_category.entry(finding.category.as_str()).or_default() += 1;
        }
        for assessment in report.requirements.iter().flat_map(|r| &r.assessments) {
            match assessment.status {
                CriterionStatus::Met => stats.criteria.met += 1,
                CriterionStatus::NotMet => stats.criteria.not_met += 1,
                CriterionStatus::Unclear => stats.criteria.unclear += 1,
            }
        }
        stats.failed_checks += report.checks.iter().filter(|c| c.is_failed()).count() as u64;
        stats.estimated_cost_usd += report.estimated_cost_usd.unwrap_or(0.0);
    }
    let gated = stats.passed + stats.failed;
    stats.pass_rate = (gated > 0).then(|| stats.passed as f64 / gated as f64);
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::domain::fixtures::{at, finding, report_view, stored};
    use crate::review::application::Severity;

    #[test]
    fn aggregates_only_runs_inside_the_period() {
        let report = report_view(
            vec![
                finding(Severity::Critical, Category::Security, "a"),
                finding(Severity::Minor, Category::Testing, "b"),
            ],
            &[CriterionStatus::Met, CriterionStatus::NotMet, CriterionStatus::Unclear],
            1,
        );
        let runs = vec![
            stored(1, "a", at(10, 0), RunStatus::Passed, Some(report.clone())),
            stored(2, "a", at(11, 0), RunStatus::Failed, Some(report)),
            stored(3, "b", at(12, 0), RunStatus::Error, None),
            stored(4, "b", at(1, 0), RunStatus::Failed, None),
        ];

        let stats = period_stats(&runs, at(5, 0));

        assert_eq!((stats.runs, stats.passed, stats.failed, stats.errored), (3, 1, 1, 1));
        assert_eq!(stats.pass_rate, Some(0.5));
        assert_eq!(stats.findings_by_severity.critical, 2);
        assert_eq!(stats.findings_by_severity.minor, 2);
        assert_eq!(stats.findings_by_category["security"], 2);
        assert_eq!(stats.findings_by_category["concurrency"], 0);
        assert_eq!(stats.findings_by_category.len(), Category::ALL.len());
        assert_eq!(stats.criteria, CriteriaCounts { met: 2, not_met: 2, unclear: 2 });
        assert_eq!(stats.failed_checks, 2);
        assert!((stats.estimated_cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn pass_rate_is_none_without_gated_runs() {
        let runs = vec![stored(1, "a", at(10, 0), RunStatus::Error, None)];
        assert_eq!(period_stats(&runs, at(1, 0)).pass_rate, None);
        assert_eq!(period_stats(&[], at(1, 0)).runs, 0);
    }
}
