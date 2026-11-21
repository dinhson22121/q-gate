//! Jira-style ticket keys (`PROJECT-123`) and their extraction from branch names and commit messages.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::commit_keys::extract_implemented_keys;

/// Minimum length of the project part (an uppercase letter followed by at least one more character).
const MIN_PROJECT_LEN: usize = 2;
/// Maximum length of the project part (an uppercase letter followed by at most nine characters).
const MAX_PROJECT_LEN: usize = 10;
/// Upper bound on issue number digits; keeps the value comfortably inside `u64`.
const MAX_NUMBER_DIGITS: usize = 12;

/// Common identifiers that look like ticket keys but are not (`UTF-8`, `SHA-256`, `ISO-8601`...).
/// Only consulted when no allow-list is configured.
const DENY_LIST: &[&str] = &[
    "AES", "ASCII", "CVE", "ECMA", "ES", "GPT", "HTTP", "IEC", "IEEE", "IPV", "ISO", "MD", "PEP", "RFC", "RSA", "SHA",
    "SSL", "TLS", "UCS", "UTF", "WIN",
];

/// A validated ticket key such as `ABC-123`, always stored in uppercase.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TicketKey {
    value: String,
    project_len: usize,
}

/// Returned by [`TicketKey::parse`] when the input is not a well-formed key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid ticket key {0:?}: expected PROJECT-123")]
pub struct InvalidTicketKey(pub String);

impl TicketKey {
    /// Parses a key like `ABC-123` (case-insensitive, normalized to uppercase).
    pub fn parse(input: &str) -> Result<Self, InvalidTicketKey> {
        let invalid = || InvalidTicketKey(input.to_string());
        let (project, number) = input.trim().split_once('-').ok_or_else(invalid)?;
        Self::from_parts(project, number).ok_or_else(invalid)
    }

    /// The project part, e.g. `ABC` for `ABC-123`.
    pub fn project(&self) -> &str {
        &self.value[..self.project_len]
    }

    /// The full key, e.g. `ABC-123`.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    fn from_parts(project: &str, number: &str) -> Option<Self> {
        if !is_valid_project(project) || !is_valid_number(number) {
            return None;
        }
        let project = project.to_ascii_uppercase();
        Some(Self { value: format!("{project}-{number}"), project_len: project.len() })
    }
}

impl fmt::Display for TicketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

impl TryFrom<String> for TicketKey {
    type Error = InvalidTicketKey;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TicketKey> for String {
    fn from(key: TicketKey) -> Self {
        key.value
    }
}

fn is_valid_project(project: &str) -> bool {
    let mut chars = project.chars();
    let starts_with_letter = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    (MIN_PROJECT_LEN..=MAX_PROJECT_LEN).contains(&project.len())
        && starts_with_letter
        && chars.all(|c| c.is_ascii_alphanumeric())
}

fn is_valid_number(number: &str) -> bool {
    !number.is_empty()
        && number.len() <= MAX_NUMBER_DIGITS
        && !number.starts_with('0')
        && number.bytes().all(|b| b.is_ascii_digit())
}

/// Finds the ticket key a change belongs to: the branch name first, then the commit message.
///
/// Branch names are matched case-insensitively (`feature/abc-123-login` → `ABC-123`); commit
/// messages require an uppercase key (`ABC-123: fix`, `[ABC-123] fix`) to avoid prose false positives.
/// When `allowed_projects` is non-empty only those projects match; otherwise a small deny-list
/// filters out look-alikes such as `UTF-8` or `SHA-256`. A key the message declares (see
/// [`extract_implemented_keys`]) wins over one it merely mentions.
pub fn extract_ticket_key(
    branch: Option<&str>,
    commit_message: Option<&str>,
    allowed_projects: &[String],
) -> Option<TicketKey> {
    branch_key(branch, allowed_projects).or_else(|| {
        let messages: Vec<String> = commit_message.map(str::to_string).into_iter().collect();
        let keys = extract_implemented_keys(None, &messages, allowed_projects);
        keys.implements.into_iter().chain(keys.references).next()
    })
}

/// The first acceptable key in a branch name (case-insensitive).
pub(super) fn branch_key(branch: Option<&str>, allowed_projects: &[String]) -> Option<TicketKey> {
    branch.and_then(|b| find_keys(b, true, allowed_projects).into_iter().next())
}

/// Every acceptable key in `text`, in order. Candidates must be delimited by non-alphanumeric
/// characters on both sides (so `fooABC-1` and `ABC-1x` are rejected).
pub(super) fn find_keys(text: &str, case_insensitive: bool, allowed_projects: &[String]) -> Vec<TicketKey> {
    let bytes = text.as_bytes();
    let mut keys = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let word_end = alnum_run_end(bytes, start);
        if word_end == start {
            start += 1;
            continue;
        }
        if let Some(key) =
            candidate_at(text, start, word_end, case_insensitive).filter(|key| is_acceptable(key, allowed_projects))
        {
            keys.push(key);
        }
        start = word_end;
    }
    keys
}

/// Parses a whole token (e.g. `SHOP-7`) as an uppercase, acceptable key.
pub(super) fn exact_key(token: &str, allowed_projects: &[String]) -> Option<TicketKey> {
    let (project, number) = token.split_once('-')?;
    if project.bytes().any(|b| b.is_ascii_lowercase()) {
        return None;
    }
    TicketKey::from_parts(project, number).filter(|key| is_acceptable(key, allowed_projects))
}

/// Tries to read `PROJECT-NUMBER` where `PROJECT` is `text[start..project_end]`.
fn candidate_at(text: &str, start: usize, project_end: usize, case_insensitive: bool) -> Option<TicketKey> {
    let bytes = text.as_bytes();
    if bytes.get(project_end) != Some(&b'-') {
        return None;
    }
    let number_end = alnum_run_end(bytes, project_end + 1);
    let project = &text[start..project_end];
    let number = &text[project_end + 1..number_end];
    if !case_insensitive && project.bytes().any(|b| b.is_ascii_lowercase()) {
        return None;
    }
    TicketKey::from_parts(project, number)
}

fn alnum_run_end(bytes: &[u8], from: usize) -> usize {
    bytes[from.min(bytes.len())..]
        .iter()
        .position(|b| !b.is_ascii_alphanumeric())
        .map_or(bytes.len(), |offset| from + offset)
}

fn is_acceptable(key: &TicketKey, allowed_projects: &[String]) -> bool {
    if allowed_projects.is_empty() {
        return !DENY_LIST.contains(&key.project());
    }
    allowed_projects.iter().any(|p| p.trim().eq_ignore_ascii_case(key.project()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(branch: Option<&str>, message: Option<&str>) -> Option<String> {
        extract_ticket_key(branch, message, &[]).map(|k| k.to_string())
    }

    fn allowed(projects: &[&str]) -> Vec<String> {
        projects.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn parses_and_normalizes_valid_keys() {
        let k = TicketKey::parse(" abc-123 ").unwrap();
        assert_eq!(k.as_str(), "ABC-123");
        assert_eq!(k.project(), "ABC");
        assert_eq!(TicketKey::parse("A1B2-7").unwrap().to_string(), "A1B2-7");
    }

    #[test]
    fn rejects_malformed_keys() {
        for bad in
            ["ABC", "ABC-", "-12", "A-1", "1AB-2", "ABC-012", "ABCDEFGHIJK-1", "ABC-12a", "AB_C-1", "ABC-1234567890123"]
        {
            assert!(TicketKey::parse(bad).is_err(), "{bad} should be invalid");
        }
        assert_eq!(TicketKey::parse("x").unwrap_err().to_string(), "invalid ticket key \"x\": expected PROJECT-123");
    }

    #[test]
    fn extracts_from_common_branch_formats() {
        let cases = [
            ("feature/ABC-123-login", "ABC-123"),
            ("abc-123", "ABC-123"),
            ("bugfix/abc-42_fix_crash", "ABC-42"),
            ("ABC-7", "ABC-7"),
            ("users/jane/PROJ2-99/refactor", "PROJ2-99"),
            ("hotfix/XY-1.patch", "XY-1"),
        ];
        for (branch, expected) in cases {
            assert_eq!(key(Some(branch), None).as_deref(), Some(expected), "branch {branch}");
        }
    }

    #[test]
    fn falls_back_to_commit_message() {
        assert_eq!(key(Some("main"), Some("ABC-123: fix login")).as_deref(), Some("ABC-123"));
        assert_eq!(key(None, Some("[ABC-9] tidy")).as_deref(), Some("ABC-9"));
        assert_eq!(key(Some("develop"), Some("Fix crash (refs OPS-1001)")).as_deref(), Some("OPS-1001"));
    }

    #[test]
    fn branch_wins_over_commit_message() {
        assert_eq!(key(Some("feature/ABC-1"), Some("XYZ-2: other")).as_deref(), Some("ABC-1"));
    }

    #[test]
    fn commit_message_requires_uppercase_key() {
        assert_eq!(key(None, Some("bump abc-12 in docs")), None);
    }

    #[test]
    fn ignores_false_positives() {
        for message in [
            "Use UTF-8 everywhere",
            "Verify SHA-256 sums",
            "Dates in ISO-8601",
            "see RFC-7231",
            "ABC-0 is zero",
            "fooABC-12",
            "ABC-12x",
        ] {
            assert_eq!(key(None, Some(message)), None, "{message}");
        }
        assert_eq!(key(None, Some("Use UTF-8 for PAY-12")).as_deref(), Some("PAY-12"));
    }

    #[test]
    fn respects_allow_list() {
        let projects = allowed(&["pay", "OPS"]);
        assert_eq!(
            extract_ticket_key(Some("feature/ABC-1-x"), Some("PAY-5: fix"), &projects).map(|k| k.to_string()),
            Some("PAY-5".into())
        );
        assert_eq!(extract_ticket_key(Some("release-2"), Some("ABC-3"), &projects), None);
        let iso = allowed(&["ISO"]);
        assert_eq!(extract_ticket_key(None, Some("ISO-9001"), &iso).map(|k| k.to_string()), Some("ISO-9001".into()));
    }

    #[test]
    fn handles_empty_and_non_ascii_input() {
        assert_eq!(key(None, None), None);
        assert_eq!(key(Some(""), Some("")), None);
        assert_eq!(key(Some("tính-năng/ABC-5"), None).as_deref(), Some("ABC-5"));
        assert_eq!(key(Some("ABC-"), None), None);
    }
}
