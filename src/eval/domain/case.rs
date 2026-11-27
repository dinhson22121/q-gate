use serde::{Deserialize, Serialize};

use crate::review::application::{AcceptanceCriterion, Category, CriterionStatus, Requirements, Severity};

pub const DEFAULT_LINE_TOLERANCE: u32 = 3;
pub const MAX_LINE_TOLERANCE: u32 = 50;
pub const MAX_ID_LEN: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid case: {0}")]
pub struct CaseError(pub String);

fn invalid<T>(message: impl Into<String>) -> Result<T, CaseError> {
    Err(CaseError(message.into()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenCase {
    pub id: String,
    #[serde(default)]
    pub description: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<CaseTicket>,
    #[serde(default, rename = "ac", skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<CaseCriterion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<ExpectedFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden: Vec<ForbiddenFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseTicket {
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseCriterion {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<ExpectedStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedStatus {
    Met,
    NotMet,
}

impl ExpectedStatus {
    pub fn matches(self, status: CriterionStatus) -> bool {
        matches!((self, status), (Self::Met, CriterionStatus::Met) | (Self::NotMet, CriterionStatus::NotMet))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFinding {
    pub file: String,
    pub line: u32,
    #[serde(default = "default_tolerance")]
    pub line_tolerance: u32,
    pub category: Category,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_severity: Option<Severity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForbiddenFinding {
    pub file: String,
    pub line: u32,
    #[serde(default = "default_tolerance")]
    pub line_tolerance: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_severity: Option<Severity>,
}

fn default_tolerance() -> u32 {
    DEFAULT_LINE_TOLERANCE
}

impl ExpectedFinding {
    pub fn label(&self) -> String {
        format!("{}:{} {}", self.file, self.line, self.category)
    }
}

impl GoldenCase {
    pub fn requirements(&self) -> Option<Requirements> {
        let ticket = self.ticket.as_ref().filter(|_| !self.criteria.is_empty())?;
        let criteria = self
            .criteria
            .iter()
            .enumerate()
            .map(|(index, criterion)| AcceptanceCriterion::new(criterion_id(&ticket.key, index), &criterion.text))
            .collect();
        Some(Requirements { key: ticket.key.clone(), title: ticket.title.clone(), url: None, criteria })
    }

    pub fn labelled_criteria(&self) -> usize {
        self.criteria.iter().filter(|c| c.expect.is_some()).count()
    }
}

pub fn criterion_id(ticket_key: &str, index: usize) -> String {
    format!("{ticket_key}/AC-{}", index + 1)
}

pub fn parse_case(text: &str) -> Result<GoldenCase, CaseError> {
    let case: GoldenCase = toml::from_str(text).map_err(|e| CaseError(e.message().trim().to_string()))?;
    validate_case(&case)?;
    Ok(case)
}

pub fn render_case(case: &GoldenCase) -> Result<String, CaseError> {
    validate_case(case)?;
    toml::to_string(case).map_err(|e| CaseError(e.to_string()))
}

pub fn validate_case_id(id: &str) -> Result<(), CaseError> {
    let valid_chars = id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if id.is_empty() || id.len() > MAX_ID_LEN || !valid_chars || id.starts_with('-') {
        return invalid(format!("case id '{id}' must be 1-{MAX_ID_LEN} characters of a-z, 0-9 and '-'"));
    }
    Ok(())
}

pub fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn validate_case(case: &GoldenCase) -> Result<(), CaseError> {
    validate_case_id(&case.id)?;
    if case.language.trim().is_empty() {
        return invalid("language must not be empty");
    }
    validate_ticket(case)?;
    for expected in &case.expected {
        validate_location(&expected.file, expected.line, expected.line_tolerance)?;
    }
    for forbidden in &case.forbidden {
        validate_location(&forbidden.file, forbidden.line, forbidden.line_tolerance)?;
    }
    if case.expected.is_empty() && case.forbidden.is_empty() && case.labelled_criteria() == 0 {
        return invalid("a case needs at least one [[expected]], [[forbidden]] or [[ac]] with an expect value");
    }
    Ok(())
}

fn validate_ticket(case: &GoldenCase) -> Result<(), CaseError> {
    match &case.ticket {
        None if !case.criteria.is_empty() => invalid("[[ac]] entries need a [ticket] section"),
        Some(ticket) if ticket.key.trim().is_empty() || ticket.key.contains(char::is_whitespace) => {
            invalid("ticket.key must be a non-empty key without whitespace")
        }
        _ if case.criteria.iter().any(|c| c.text.trim().is_empty()) => invalid("ac.text must not be empty"),
        _ => Ok(()),
    }
}

fn validate_location(file: &str, line: u32, tolerance: u32) -> Result<(), CaseError> {
    if !is_safe_relative_path(file) {
        return invalid(format!("file '{file}' must be a relative path inside the repository"));
    }
    if line == 0 {
        return invalid(format!("{file}: line must be at least 1"));
    }
    if tolerance > MAX_LINE_TOLERANCE {
        return invalid(format!("{file}: line_tolerance must be at most {MAX_LINE_TOLERANCE}"));
    }
    Ok(())
}

pub fn language_of(paths: &[String]) -> String {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for path in paths {
        let extension = path.rsplit('.').next().unwrap_or_default();
        let Some(language) = language_for_extension(extension) else { continue };
        match counts.iter_mut().find(|(name, _)| *name == language) {
            Some((_, count)) => *count += 1,
            None => counts.push((language, 1)),
        }
    }
    counts.into_iter().max_by_key(|(_, count)| *count).map_or("unknown", |(name, _)| name).to_string()
}

fn language_for_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "go" => Some("go"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "rs" => Some("rust"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
id = "go-demo"
description = "demo"
language = "go"

[ticket]
key = "KAN-1"
title = "Demo"

[[ac]]
text = "first"
expect = "met"

[[ac]]
text = "second"

[[expected]]
file = "main.go"
line = 12
category = "security"
min_severity = "major"

[[forbidden]]
file = "main.go"
line = 30
line_tolerance = 5
"#;

    #[test]
    fn parses_a_full_case_with_defaults() {
        let case = parse_case(FULL).unwrap();

        assert_eq!(case.id, "go-demo");
        assert_eq!(case.expected[0].line_tolerance, DEFAULT_LINE_TOLERANCE);
        assert_eq!(case.expected[0].min_severity, Some(Severity::Major));
        assert_eq!(case.forbidden[0].line_tolerance, 5);
        assert_eq!(case.forbidden[0].category, None);
        assert_eq!(case.criteria[0].expect, Some(ExpectedStatus::Met));
        assert_eq!(case.criteria[1].expect, None);
        assert_eq!(case.labelled_criteria(), 1);
    }

    #[test]
    fn requirements_number_the_criteria_under_the_ticket_key() {
        let requirements = parse_case(FULL).unwrap().requirements().unwrap();

        assert_eq!(requirements.key, "KAN-1");
        let ids: Vec<&str> = requirements.criteria.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["KAN-1/AC-1", "KAN-1/AC-2"]);
    }

    #[test]
    fn a_case_without_criteria_has_no_requirements() {
        let text = "id = \"a\"\nlanguage = \"go\"\n[[expected]]\nfile = \"a.go\"\nline = 1\ncategory = \"security\"\n";

        assert!(parse_case(text).unwrap().requirements().is_none());
    }

    #[test]
    fn rendering_round_trips() {
        let case = parse_case(FULL).unwrap();

        assert_eq!(parse_case(&render_case(&case).unwrap()).unwrap(), case);
    }

    #[test]
    fn rejects_bad_cases() {
        let with = |extra: &str| format!("id = \"a\"\nlanguage = \"go\"\n{extra}");
        let expected = |file: &str, line: u32, tolerance: u32| {
            with(&format!(
                "[[expected]]\nfile = \"{file}\"\nline = {line}\nline_tolerance = {tolerance}\ncategory = \"security\"\n"
            ))
        };
        let cases = [
            (with(""), "at least one"),
            (expected("../x.go", 1, 3), "relative path"),
            (expected("/etc/passwd", 1, 3), "relative path"),
            (expected("a.go", 0, 3), "line must be at least 1"),
            (expected("a.go", 1, 51), "line_tolerance"),
            (with("[[ac]]\ntext = \"x\"\nexpect = \"met\"\n"), "need a [ticket]"),
            (with("[ticket]\nkey = \"A B\"\n[[ac]]\ntext = \"x\"\nexpect = \"met\"\n"), "ticket.key"),
            (with("[ticket]\nkey = \"A-1\"\n[[ac]]\ntext = \" \"\nexpect = \"met\"\n"), "ac.text"),
            (with("[[ac]]\nexpect = \"unclear\"\n"), "unclear"),
            (with("[[bogus]]\n"), "bogus"),
            ("id = \"Bad_ID\"\nlanguage = \"go\"\n".into(), "case id"),
            ("id = \"a\"\nlanguage = \" \"\n".into(), "language"),
        ];
        for (text, needle) in cases {
            let error = parse_case(&text).unwrap_err();
            assert!(error.0.contains(needle), "{needle}: {error}");
        }
    }

    #[test]
    fn case_ids_are_restricted() {
        assert!(validate_case_id("go-kan-5").is_ok());
        for bad in ["", "-a", "A", "a_b", "a/b", "..", &"a".repeat(MAX_ID_LEN + 1)] {
            assert!(validate_case_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn safe_paths_stay_inside_the_tree() {
        for good in ["a.go", "src/a.go", "a/b/c.py"] {
            assert!(is_safe_relative_path(good), "{good}");
        }
        for bad in ["", "/a", "../a", "a/../b", "a//b", "./a", "a\\b", "a/"] {
            assert!(!is_safe_relative_path(bad), "{bad}");
        }
    }

    #[test]
    fn language_is_the_most_common_known_extension() {
        let paths = |items: &[&str]| items.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(language_of(&paths(&["a.go", "b.go", "c.py", "README.md"])), "go");
        assert_eq!(language_of(&paths(&["a.tsx"])), "typescript");
        assert_eq!(language_of(&paths(&["a.md"])), "unknown");
    }

    #[test]
    fn expected_status_matches_only_its_own_status() {
        assert!(ExpectedStatus::Met.matches(CriterionStatus::Met));
        assert!(!ExpectedStatus::Met.matches(CriterionStatus::Unclear));
        assert!(ExpectedStatus::NotMet.matches(CriterionStatus::NotMet));
        assert!(!ExpectedStatus::NotMet.matches(CriterionStatus::Met));
    }
}
