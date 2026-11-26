use serde::{Deserialize, Serialize};

use super::finding::{Finding, FindingSource, Severity};

const MAX_FIELD_CHARS: usize = 800;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reproduce {
    pub input: String,
    pub expected: String,
    pub actual: String,
    #[serde(default)]
    pub command: Option<String>,
}

impl Reproduce {
    pub fn new(input: &str, expected: &str, actual: &str, command: &str) -> Option<Self> {
        let (input, expected, actual) = (clip(input), clip(expected), clip(actual));
        if input.is_empty() || expected.is_empty() || actual.is_empty() {
            return None;
        }
        if normalized(&expected) == normalized(&actual) {
            return None;
        }
        let command = clip(command);
        Some(Self { input, expected, actual, command: (!command.is_empty()).then_some(command) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Demotion {
    MissingReproduce,
}

impl Demotion {
    pub fn describe(self) -> &'static str {
        match self {
            Self::MissingReproduce => {
                "reported as critical without concrete reproduce steps (input, expected, actual), so it counts as major"
            }
        }
    }
}

pub fn demote_unproven(findings: Vec<Finding>) -> Vec<Finding> {
    findings.into_iter().map(demote_if_unproven).collect()
}

fn demote_if_unproven(finding: Finding) -> Finding {
    let unproven =
        finding.source == FindingSource::Llm && finding.severity == Severity::Critical && finding.reproduce.is_none();
    if !unproven {
        return finding;
    }
    Finding { severity: Severity::Major, demoted: Some(Demotion::MissingReproduce), ..finding }
}

fn clip(text: &str) -> String {
    text.trim().chars().take(MAX_FIELD_CHARS).collect()
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::finding::fixtures::finding;

    fn proof() -> Reproduce {
        Reproduce::new("GET /users?id=1' OR '1'='1", "400 Bad Request", "200 with every user row", "").unwrap()
    }

    #[test]
    fn reproduce_requires_input_expected_and_actual() {
        assert!(Reproduce::new("", "a", "b", "").is_none());
        assert!(Reproduce::new("i", "  ", "b", "").is_none());
        assert!(Reproduce::new("i", "a", "\n", "").is_none());
        let full = Reproduce::new(" call f(0) ", "returns 0", "panics", " go test ./x ").unwrap();
        assert_eq!(full.input, "call f(0)");
        assert_eq!(full.command.as_deref(), Some("go test ./x"));
        assert_eq!(Reproduce::new("i", "a", "b", "  ").unwrap().command, None);
    }

    #[test]
    fn reproduce_where_expected_equals_actual_proves_nothing() {
        assert!(Reproduce::new("call f", "Returns  0", "returns 0", "").is_none());
    }

    #[test]
    fn reproduce_fields_are_clipped() {
        let long = "x".repeat(MAX_FIELD_CHARS + 10);
        let r = Reproduce::new(&long, "a", "b", &long).unwrap();
        assert_eq!(r.input.chars().count(), MAX_FIELD_CHARS);
        assert_eq!(r.command.unwrap().chars().count(), MAX_FIELD_CHARS);
    }

    #[test]
    fn llm_critical_without_reproduce_is_demoted_to_major() {
        let demoted = demote_unproven(vec![finding("a.go", 1, Severity::Critical, "x")]).remove(0);
        assert_eq!(demoted.severity, Severity::Major);
        assert_eq!(demoted.demoted, Some(Demotion::MissingReproduce));
    }

    #[test]
    fn proven_critical_and_lower_severities_are_untouched() {
        let proven = Finding { reproduce: Some(proof()), ..finding("a.go", 1, Severity::Critical, "x") };
        let major = finding("a.go", 2, Severity::Major, "x");
        let before = vec![proven, major];
        assert_eq!(demote_unproven(before.clone()), before);
    }

    #[test]
    fn static_tool_critical_needs_no_reproduce() {
        let tool = Finding {
            source: FindingSource::StaticTool("gitleaks".into()),
            ..finding("a.go", 1, Severity::Critical, "x")
        };
        assert_eq!(demote_unproven(vec![tool.clone()]), vec![tool]);
    }

    #[test]
    fn demotion_has_a_description() {
        assert!(Demotion::MissingReproduce.describe().contains("reproduce"));
    }
}
