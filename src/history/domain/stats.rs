use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::report_view::ReportView;
use super::run::{RunStatus, SeverityCounts, StoredRun};
use crate::review::application::{Category, CriterionStatus, HumanMark, UnverifiableReason};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CriteriaCounts {
    pub met: u64,
    pub not_met: u64,
    pub unclear: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UnverifiableStats {
    pub total: u64,
    pub by_reason: BTreeMap<&'static str, u64>,
    pub runs_with_unverifiable: u64,
    pub reported_runs: u64,
    pub run_rate: Option<f64>,
    pub human_verified: u64,
}

impl UnverifiableStats {
    fn new() -> Self {
        Self { by_reason: UnverifiableReason::ALL.iter().map(|r| (r.as_str(), 0)).collect(), ..Self::default() }
    }

    fn record(&mut self, report: &ReportView) {
        self.reported_runs += 1;
        self.total += report.unverifiable.len() as u64;
        self.runs_with_unverifiable += u64::from(!report.unverifiable.is_empty());
        for item in &report.unverifiable {
            *self.by_reason.entry(item.reason.as_str()).or_default() += 1;
        }
        let assessments = report.requirements.iter().flat_map(|r| &r.assessments);
        self.human_verified += assessments.filter(|a| a.human == Some(HumanMark::VerifiedMet)).count() as u64;
    }

    fn finish(&mut self) {
        self.run_rate =
            (self.reported_runs > 0).then(|| self.runs_with_unverifiable as f64 / self.reported_runs as f64);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodStats {
    pub since: DateTime<Utc>,
    pub runs: u64,
    pub passed: u64,
    pub failed: u64,
    pub errored: u64,
    pub pass_rate: Option<f64>,
    pub findings_by_severity: SeverityCounts,
    pub findings_by_category: BTreeMap<&'static str, u64>,
    pub criteria: CriteriaCounts,
    pub failed_checks: u64,
    pub estimated_cost_usd: f64,
    pub unverifiable: UnverifiableStats,
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
        unverifiable: UnverifiableStats::new(),
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
        stats.unverifiable.record(report);
    }
    let gated = stats.passed + stats.failed;
    stats.unverifiable.finish();
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

    fn unverifiable_item(reason: UnverifiableReason) -> crate::review::application::Unverifiable {
        crate::review::application::Unverifiable {
            subject: crate::review::application::UnverifiableSubject::Criterion,
            id: "AC0".into(),
            reason,
            detail: String::new(),
            retried: false,
            remedy: String::new(),
        }
    }

    #[test]
    fn counts_unverifiable_items_by_reason_and_runs_affected() {
        let mut flagged = report_view(Vec::new(), &[CriterionStatus::Met, CriterionStatus::Met], 0);
        flagged.unverifiable =
            vec![unverifiable_item(UnverifiableReason::NoEvidence), unverifiable_item(UnverifiableReason::NoEvidence)];
        flagged.requirements.as_mut().unwrap().assessments[0].human = Some(HumanMark::VerifiedMet);
        flagged.requirements.as_mut().unwrap().assessments[1].human = Some(HumanMark::NotApplicable);
        let mut other = report_view(Vec::new(), &[], 0);
        other.unverifiable = vec![unverifiable_item(UnverifiableReason::TestTimeout)];
        let clean = report_view(Vec::new(), &[], 0);
        let runs = vec![
            stored(1, "a", at(10, 0), RunStatus::Failed, Some(flagged)),
            stored(2, "a", at(11, 0), RunStatus::Failed, Some(other)),
            stored(3, "a", at(12, 0), RunStatus::Passed, Some(clean)),
            stored(4, "a", at(13, 0), RunStatus::Error, None),
        ];

        let stats = period_stats(&runs, at(1, 0)).unverifiable;

        assert_eq!((stats.total, stats.runs_with_unverifiable, stats.reported_runs), (3, 2, 3));
        assert_eq!(stats.by_reason["no_evidence"], 2);
        assert_eq!(stats.by_reason["test_timeout"], 1);
        assert_eq!(stats.by_reason["tool_error"], 0);
        assert_eq!(stats.by_reason.len(), UnverifiableReason::ALL.len());
        assert_eq!(stats.human_verified, 1);
        assert!((stats.run_rate.unwrap() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn unverifiable_rate_is_none_without_reported_runs() {
        let runs = vec![stored(1, "a", at(10, 0), RunStatus::Error, None)];
        assert_eq!(period_stats(&runs, at(1, 0)).unverifiable.run_rate, None);
    }

    #[test]
    fn pass_rate_is_none_without_gated_runs() {
        let runs = vec![stored(1, "a", at(10, 0), RunStatus::Error, None)];
        assert_eq!(period_stats(&runs, at(1, 0)).pass_rate, None);
        assert_eq!(period_stats(&[], at(1, 0)).runs, 0);
    }
}
