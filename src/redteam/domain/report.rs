use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::finding::Finding;
use super::target::RedactedTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    Completed,
    Aborted,
    TimedOut,
}

impl ScanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    #[serde(default)]
    pub id: Option<i64>,
    pub target: RedactedTarget,
    pub findings: Vec<Finding>,
    pub notes: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub cost_usd: f64,
    pub http_requests: u32,
    pub model_calls: u32,
    pub status: ScanStatus,
}

impl ScanReport {
    pub fn critical_findings(&self) -> impl Iterator<Item = &Finding> {
        use super::severity::Severity;
        self.findings.iter().filter(|f| f.severity == Severity::Critical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redteam::domain::category::AttackCategory;
    use crate::redteam::domain::finding::Finding;
    use crate::redteam::domain::poc::Poc;
    use crate::redteam::domain::rule::prevention_rule;
    use crate::redteam::domain::severity::Severity;
    use crate::redteam::domain::surface::{Endpoint, EndpointSource};
    use crate::redteam::domain::target::ScanTarget;

    fn finding(severity: Severity) -> Finding {
        Finding {
            category: AttackCategory::SqlInjection,
            severity,
            endpoint: Endpoint::new("GET", "/x", EndpointSource::CommonPath),
            poc: Poc { request: "r".into(), response_evidence: "e".into(), reproduce: "p".into() },
            impact: "i".into(),
            fix: "f".into(),
            rule: prevention_rule(AttackCategory::SqlInjection, "f"),
        }
    }

    #[test]
    fn only_critical_findings_are_counted_as_critical() {
        let report = ScanReport {
            id: None,
            target: ScanTarget::default().redacted(),
            findings: vec![finding(Severity::Critical), finding(Severity::High)],
            notes: vec![],
            started_at: Utc::now(),
            finished_at: Utc::now(),
            cost_usd: 0.0,
            http_requests: 0,
            model_calls: 0,
            status: ScanStatus::Completed,
        };
        assert_eq!(report.critical_findings().count(), 1);
    }
}
