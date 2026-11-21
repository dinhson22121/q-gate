//! "Every repeated review comment is a candidate for a deterministic lint rule":
//! group accepted findings across runs by (category, normalized title).

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::run::StoredRun;
use crate::review::application::Category;

pub const RECURRING_THRESHOLD: u64 = 3;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecurringIssue {
    pub category: Category,
    pub normalized_title: String,
    pub occurrences: u64,
    pub repos: Vec<String>,
    pub last_seen: DateTime<Utc>,
    /// Original title of the most recent occurrence.
    pub example_title: String,
    pub example_file: String,
    pub example_evidence: String,
}

/// Lowercase; drop `code spans`, quoted strings and digits; collapse whitespace
/// and trim trailing punctuation, so "Unwrap on `foo` at line 12" and
/// "unwrap on `bar` at line 7" fall into the same group.
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut closing: Option<char> = None;
    for c in title.chars() {
        if let Some(end) = closing {
            if c == end {
                closing = None;
                out.push(' ');
            }
            continue;
        }
        match c {
            '`' | '"' | '\u{201C}' | '\u{2018}' => closing = Some(matching_quote(c)),
            '\'' if !out.ends_with(|p: char| p.is_alphanumeric()) => closing = Some('\''),
            c if c.is_ascii_digit() => out.push(' '),
            c => out.extend(c.to_lowercase()),
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_string()
}

fn matching_quote(open: char) -> char {
    match open {
        '\u{201C}' => '\u{201D}',
        '\u{2018}' => '\u{2019}',
        other => other,
    }
}

/// Groups with at least `threshold` occurrences, most frequent first
/// (ties: most recently seen first).
pub fn find_recurring(runs: &[StoredRun], threshold: u64) -> Vec<RecurringIssue> {
    let mut groups: HashMap<(Category, String), (RecurringIssue, BTreeSet<String>)> = HashMap::new();
    for run in runs {
        let Some(report) = &run.report else { continue };
        for finding in &report.findings {
            let key = normalize_title(&finding.title);
            if key.is_empty() {
                continue;
            }
            let (issue, repos) = groups.entry((finding.category, key.clone())).or_insert_with(|| {
                let issue = RecurringIssue {
                    category: finding.category,
                    normalized_title: key,
                    occurrences: 0,
                    repos: Vec::new(),
                    last_seen: run.created_at,
                    example_title: finding.title.clone(),
                    example_file: finding.file.clone(),
                    example_evidence: finding.evidence.clone(),
                };
                (issue, BTreeSet::new())
            });
            issue.occurrences += 1;
            repos.insert(run.repo_name.clone());
            if run.created_at > issue.last_seen {
                issue.last_seen = run.created_at;
                issue.example_title.clone_from(&finding.title);
                issue.example_file.clone_from(&finding.file);
                issue.example_evidence.clone_from(&finding.evidence);
            }
        }
    }
    let mut issues: Vec<RecurringIssue> = groups
        .into_values()
        .filter(|(issue, _)| issue.occurrences >= threshold)
        .map(|(issue, repos)| RecurringIssue { repos: repos.into_iter().collect(), ..issue })
        .collect();
    issues.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.normalized_title.cmp(&b.normalized_title))
    });
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::domain::fixtures::{at, finding, report_view, stored};
    use crate::history::domain::run::RunStatus;
    use crate::review::application::Severity;

    #[test]
    fn normalizes_identifiers_quotes_and_digits() {
        assert_eq!(normalize_title("Unwrap on `foo` at line 12."), "unwrap on at line");
        assert_eq!(normalize_title("unwrap on `bar_baz()` at line 7"), "unwrap on at line");
        assert_eq!(normalize_title("Missing check for \"id\" and 'name'"), "missing check for and");
        assert_eq!(normalize_title("Don't   panic"), "don't panic");
        assert_eq!(normalize_title("`x`"), "");
    }

    #[test]
    fn groups_by_category_and_normalized_title_with_threshold() {
        let sql = |n: &str| finding(Severity::Major, Category::Security, &format!("SQL built from `{n}`"));
        let runs = vec![
            stored(1, "api", at(1, 0), RunStatus::Failed, Some(report_view(vec![sql("a"), sql("b")], &[], 0))),
            stored(2, "web", at(3, 0), RunStatus::Failed, Some(report_view(vec![sql("c")], &[], 0))),
            stored(
                3,
                "api",
                at(2, 0),
                RunStatus::Failed,
                Some(report_view(
                    vec![
                        // same title, different category: separate group
                        finding(Severity::Major, Category::Correctness, "SQL built from `x`"),
                        finding(Severity::Minor, Category::Testing, "missing test"),
                        finding(Severity::Minor, Category::Testing, "Missing test"),
                    ],
                    &[],
                    0,
                )),
            ),
            stored(4, "api", at(4, 0), RunStatus::Error, None),
        ];

        let issues = find_recurring(&runs, RECURRING_THRESHOLD);

        assert_eq!(issues.len(), 1);
        let issue = &issues[0];
        assert_eq!(issue.category, Category::Security);
        assert_eq!(issue.normalized_title, "sql built from");
        assert_eq!(issue.occurrences, 3);
        assert_eq!(issue.repos, vec!["api".to_string(), "web".to_string()]);
        assert_eq!(issue.last_seen, at(3, 0));
        assert_eq!(issue.example_title, "SQL built from `c`");

        let two = find_recurring(&runs, 2);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].occurrences, 3);
        assert_eq!(two[1].normalized_title, "missing test");
    }
}
