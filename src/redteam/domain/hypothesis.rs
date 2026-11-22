use serde::{Deserialize, Serialize};

use super::category::AttackCategory;
use super::evidence::{AttackRequest, ExpectedEvidence};
use super::surface::Endpoint;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hypothesis {
    pub category: AttackCategory,
    pub endpoint: Endpoint,
    pub rationale: String,
    pub request: AttackRequest,
    pub expected_evidence: ExpectedEvidence,
    pub impact: String,
    pub fix: String,
    #[serde(default)]
    pub destructive: bool,
}
