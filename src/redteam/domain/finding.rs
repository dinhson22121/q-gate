use serde::{Deserialize, Serialize};

use super::category::AttackCategory;
use super::poc::Poc;
use super::rule::PreventionRule;
use super::severity::Severity;
use super::surface::Endpoint;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub category: AttackCategory,
    pub severity: Severity,
    pub endpoint: Endpoint,
    pub poc: Poc,
    pub impact: String,
    pub fix: String,
    pub rule: PreventionRule,
}
