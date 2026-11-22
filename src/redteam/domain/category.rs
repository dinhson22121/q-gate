use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackCategory {
    SqlInjection,
    CommandInjection,
    PathTraversal,
    Ssti,
    InsecureDeserialization,
    AuthBypass,
    DataExfiltration,
    Rce,
    Ssrf,
    #[serde(rename = "nosql_injection")]
    NoSqlInjection,
    XxeInjection,
}

impl AttackCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SqlInjection => "sql_injection",
            Self::CommandInjection => "command_injection",
            Self::PathTraversal => "path_traversal",
            Self::Ssti => "ssti",
            Self::InsecureDeserialization => "insecure_deserialization",
            Self::AuthBypass => "auth_bypass",
            Self::DataExfiltration => "data_exfiltration",
            Self::Rce => "rce",
            Self::Ssrf => "ssrf",
            Self::NoSqlInjection => "nosql_injection",
            Self::XxeInjection => "xxe_injection",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SqlInjection => "SQL injection",
            Self::CommandInjection => "OS command injection",
            Self::PathTraversal => "path traversal",
            Self::Ssti => "server-side template injection",
            Self::InsecureDeserialization => "insecure deserialization",
            Self::AuthBypass => "authentication bypass to data",
            Self::DataExfiltration => "data exfiltration",
            Self::Rce => "remote code execution",
            Self::Ssrf => "server-side request forgery",
            Self::NoSqlInjection => "NoSQL injection",
            Self::XxeInjection => "XML external entity injection",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        for category in [
            AttackCategory::SqlInjection,
            AttackCategory::CommandInjection,
            AttackCategory::PathTraversal,
            AttackCategory::Ssti,
            AttackCategory::InsecureDeserialization,
            AttackCategory::AuthBypass,
            AttackCategory::DataExfiltration,
            AttackCategory::Rce,
            AttackCategory::Ssrf,
            AttackCategory::NoSqlInjection,
            AttackCategory::XxeInjection,
        ] {
            let json = serde_json::to_string(&category).unwrap();
            assert_eq!(serde_json::from_str::<AttackCategory>(&json).unwrap(), category);
            assert!(!category.label().is_empty());
            assert_eq!(json, format!("\"{}\"", category.as_str()));
        }
    }
}
