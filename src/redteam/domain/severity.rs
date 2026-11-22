use serde::{Deserialize, Serialize};

use super::category::AttackCategory;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    High,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

pub fn severity_for(category: AttackCategory) -> Severity {
    match category {
        AttackCategory::DataExfiltration | AttackCategory::AuthBypass => Severity::High,
        _ => Severity::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_outranks_high() {
        assert!(Severity::Critical > Severity::High);
    }

    #[test]
    fn rce_and_sqli_are_critical_by_default() {
        assert_eq!(severity_for(AttackCategory::Rce), Severity::Critical);
        assert_eq!(severity_for(AttackCategory::SqlInjection), Severity::Critical);
        assert_eq!(severity_for(AttackCategory::CommandInjection), Severity::Critical);
        assert_eq!(severity_for(AttackCategory::PathTraversal), Severity::Critical);
    }

    #[test]
    fn auth_bypass_and_exfiltration_are_high() {
        assert_eq!(severity_for(AttackCategory::AuthBypass), Severity::High);
        assert_eq!(severity_for(AttackCategory::DataExfiltration), Severity::High);
    }

    #[test]
    fn ssrf_and_the_extra_injection_variants_are_critical() {
        assert_eq!(severity_for(AttackCategory::Ssrf), Severity::Critical);
        assert_eq!(severity_for(AttackCategory::NoSqlInjection), Severity::Critical);
        assert_eq!(severity_for(AttackCategory::XxeInjection), Severity::Critical);
    }
}
