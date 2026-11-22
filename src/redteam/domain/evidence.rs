use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttackRequest {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub elapsed: Duration,
}

impl ProbeResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedEvidence {
    BodyContains(String),
    ExtraDelayAtLeastMs(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookup_is_case_insensitive() {
        let response = ProbeResponse {
            status: 200,
            headers: vec![("Set-Cookie".into(), "a=b".into())],
            body: String::new(),
            elapsed: Duration::ZERO,
        };
        assert_eq!(response.header("set-cookie"), Some("a=b"));
        assert_eq!(response.header("missing"), None);
    }
}
