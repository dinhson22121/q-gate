use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poc {
    pub request: String,
    pub response_evidence: String,
    pub reproduce: String,
}
