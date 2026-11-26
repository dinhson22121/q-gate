use std::fmt;

use serde::{Deserialize, Serialize};

pub use super::reproduce::{Demotion, Reproduce};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Minor,
    Major,
    Critical,
}

impl Severity {
    pub const ALL: [Self; 4] = [Self::Info, Self::Minor, Self::Major, Self::Critical];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Minor => "minor",
            Self::Major => "major",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Security,
    Correctness,
    Concurrency,
    DataLoss,
    Performance,
    ErrorHandling,
    Maintainability,
    Testing,
    Architecture,
}

impl Category {
    pub const ALL: [Self; 9] = [
        Self::Security,
        Self::Correctness,
        Self::Concurrency,
        Self::DataLoss,
        Self::Performance,
        Self::ErrorHandling,
        Self::Maintainability,
        Self::Testing,
        Self::Architecture,
    ];

    pub const LLM: [Self; 8] = [
        Self::Security,
        Self::Correctness,
        Self::Concurrency,
        Self::DataLoss,
        Self::Performance,
        Self::ErrorHandling,
        Self::Maintainability,
        Self::Testing,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Security => "security",
            Self::Correctness => "correctness",
            Self::Concurrency => "concurrency",
            Self::DataLoss => "data_loss",
            Self::Performance => "performance",
            Self::ErrorHandling => "error_handling",
            Self::Maintainability => "maintainability",
            Self::Testing => "testing",
            Self::Architecture => "architecture",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

macro_rules! display_as_str {
    ($($t:ty),*) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    )*};
}
display_as_str!(Severity, Category, Confidence);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum FindingSource {
    Llm,
    StaticTool(String),
}

const MAX_RULE_CHARS: usize = 300;
const MAX_SCOPE_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreventionRule {
    pub text: String,
    pub scope: Option<String>,
}

impl PreventionRule {
    pub fn new(text: &str, scope: &str) -> Option<Self> {
        let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_RULE_CHARS).collect();
        if text.is_empty() {
            return None;
        }
        let scope = scope.trim();
        let is_glob = scope.len() <= MAX_SCOPE_CHARS
            && scope.chars().all(|c| c.is_ascii_alphanumeric() || "*?/._-{},[]!".contains(c));
        let scope = (is_glob && !scope.is_empty() && !matches!(scope, "*" | "**" | "**/*")).then(|| scope.to_string());
        Some(Self { text, scope })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub category: Category,
    pub confidence: Confidence,
    pub title: String,
    pub explanation: String,
    pub evidence: String,
    pub suggestion: Option<String>,
    pub prevention_rule: Option<PreventionRule>,
    pub source: FindingSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduce: Option<Reproduce>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demoted: Option<Demotion>,
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn finding(file: &str, line: u32, severity: Severity, evidence: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line,
            severity,
            category: Category::Correctness,
            confidence: Confidence::High,
            title: "title".into(),
            explanation: "explanation".into(),
            evidence: evidence.to_string(),
            suggestion: None,
            prevention_rule: None,
            source: FindingSource::Llm,
            reproduce: None,
            demoted: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_from_info_to_critical() {
        assert!(Severity::Critical > Severity::Major);
        assert!(Severity::Minor > Severity::Info);
    }

    #[test]
    fn serde_names_match_as_str() {
        for s in Severity::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
        for c in Category::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
        for c in Confidence::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
    }

    #[test]
    fn prevention_rule_is_one_line_and_scope_must_be_a_glob() {
        let rule = PreventionRule::new("  Always close\n rows.  ", " **/*.go ").unwrap();
        assert_eq!(rule, PreventionRule { text: "Always close rows.".into(), scope: Some("**/*.go".into()) });
        assert_eq!(PreventionRule::new(" \n ", "**/*.go"), None);
        for everywhere in ["", "**", "**/*", "*", "src/`x`", "a b", &"a".repeat(MAX_SCOPE_CHARS + 1)] {
            assert_eq!(PreventionRule::new("r", everywhere).unwrap().scope, None, "{everywhere}");
        }
        assert_eq!(
            PreventionRule::new("r", "src/{api,web}/**/*.ts").unwrap().scope.as_deref(),
            Some("src/{api,web}/**/*.ts")
        );
        assert_eq!(PreventionRule::new(&"x".repeat(MAX_RULE_CHARS + 50), "").unwrap().text.len(), MAX_RULE_CHARS);
    }

    #[test]
    fn display_uses_snake_case_name() {
        assert_eq!(Category::DataLoss.to_string(), "data_loss");
        assert_eq!(Severity::Critical.to_string(), "critical");
        assert_eq!(Confidence::Medium.to_string(), "medium");
    }
}
