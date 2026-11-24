use std::cmp::Reverse;
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::review::application::{Confidence, Finding, ReviewReport, RuleViolation, Severity};
use crate::review::domain::finding::FindingSource;

pub const RULES_SCHEMA: &str = "qgate.rules/v1";
const STATE_EXTENSION: &str = ".json";
const MARKDOWN_EXTENSION: &str = ".md";
const EXAMPLE_CHARS: usize = 120;
const ARCHITECTURE: &str = "architecture";
const TRAILING_PUNCTUATION: [char; 6] = ['.', '!', ';', ':', ',', '?'];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sighting {
    pub commit: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleExample {
    pub location: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleCandidate {
    pub text: String,
    pub scope: Option<String>,
    pub severity: Severity,
    pub category: String,
    pub example: Option<RuleExample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedRule {
    pub text: String,
    #[serde(default)]
    pub scope: Option<String>,
    pub severity: Severity,
    pub category: String,
    pub first_seen: Sighting,
    pub last_seen: Sighting,
    pub hits: u32,
    #[serde(default)]
    pub example: Option<RuleExample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleBook {
    pub schema: String,
    pub rules: Vec<LearnedRule>,
}

impl Default for RuleBook {
    fn default() -> Self {
        Self { schema: RULES_SCHEMA.into(), rules: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RuleBookError(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    pub book: RuleBook,
    pub added: Vec<RuleCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesPaths {
    pub markdown: String,
    pub state: String,
}

impl RulesPaths {
    pub fn new(repo_dir: &str, file_name: &str) -> Self {
        let stem = file_name.strip_suffix(MARKDOWN_EXTENSION).unwrap_or(file_name);
        Self { markdown: format!("{repo_dir}/{file_name}"), state: format!("{repo_dir}/.{stem}{STATE_EXTENSION}") }
    }

    pub fn file_names(&self) -> [&str; 2] {
        [&self.markdown, &self.state].map(|p| p.rsplit('/').next().unwrap_or(p))
    }
}

pub fn candidates(report: &ReviewReport) -> Vec<RuleCandidate> {
    let shown = |(index, finding): &(usize, &Finding)| {
        report.gate.blocking.contains(index) || finding.severity >= Severity::Minor
    };
    let trusted = |(_, finding): &(usize, &Finding)| {
        finding.source != FindingSource::Llm || (report.audited && finding.confidence != Confidence::Low)
    };
    let from_findings =
        report.findings.iter().enumerate().filter(shown).filter(trusted).filter_map(|(_, f)| finding_candidate(f));
    let violations = report.agents.iter().flat_map(|a| a.violations.iter()).map(violation_candidate);
    let mut out: Vec<RuleCandidate> = Vec::new();
    for candidate in from_findings.chain(violations) {
        let key = rule_key(&candidate.text, candidate.scope.as_deref());
        match out.iter_mut().find(|c| rule_key(&c.text, c.scope.as_deref()) == key) {
            Some(existing) if candidate.severity > existing.severity => *existing = candidate,
            Some(_) => {}
            None => out.push(candidate),
        }
    }
    out.sort_by(|a, b| b.severity.cmp(&a.severity).then_with(|| a.text.cmp(&b.text)));
    out
}

fn finding_candidate(finding: &Finding) -> Option<RuleCandidate> {
    let rule = finding.prevention_rule.as_ref()?;
    Some(RuleCandidate {
        text: rule.text.clone(),
        scope: rule.scope.clone(),
        severity: finding.severity,
        category: finding.category.as_str().into(),
        example: example(&finding.file, finding.line, &finding.evidence),
    })
}

fn violation_candidate(violation: &RuleViolation) -> RuleCandidate {
    let title = violation.rule_title.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if title.is_empty() { format!("Follow architecture rule {}", violation.rule_id.trim()) } else { title };
    RuleCandidate {
        text,
        scope: None,
        severity: if violation.approved { Severity::Major } else { Severity::Minor },
        category: ARCHITECTURE.into(),
        example: example(&violation.file, violation.line, &violation.evidence),
    }
}

fn example(file: &str, line: u32, evidence: &str) -> Option<RuleExample> {
    let file = file.trim();
    if file.is_empty() {
        return None;
    }
    let first = evidence.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut short: String = first.chars().take(EXAMPLE_CHARS).collect();
    if short.len() < first.len() {
        short.push('…');
    }
    Some(RuleExample { location: format!("{file}:{line}"), evidence: short })
}

pub fn rule_key(text: &str, scope: Option<&str>) -> (String, String) {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let text = text.trim_end_matches(|c: char| TRAILING_PUNCTUATION.contains(&c) || c.is_whitespace());
    (text.to_string(), scope.unwrap_or_default().trim().to_string())
}

impl RuleBook {
    pub fn from_json(bytes: &[u8]) -> Result<Self, RuleBookError> {
        let book: Self = serde_json::from_slice(bytes).map_err(|e| RuleBookError(format!("invalid JSON: {e}")))?;
        if book.schema != RULES_SCHEMA {
            return Err(RuleBookError(format!("unsupported schema '{}', expected {RULES_SCHEMA}", book.schema)));
        }
        Ok(book)
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        text.push('\n');
        text
    }

    pub fn merge(&self, candidates: &[RuleCandidate], seen: &Sighting, max: usize) -> MergeOutcome {
        let mut rules = self.rules.clone();
        let mut added = Vec::new();
        for candidate in candidates {
            let key = rule_key(&candidate.text, candidate.scope.as_deref());
            match rules.iter_mut().find(|r| rule_key(&r.text, r.scope.as_deref()) == key) {
                Some(rule) => {
                    if rule.last_seen.commit != seen.commit {
                        rule.hits = rule.hits.saturating_add(1);
                    }
                    rule.last_seen = seen.clone();
                    rule.severity = rule.severity.max(candidate.severity);
                    if candidate.example.is_some() {
                        rule.example.clone_from(&candidate.example);
                    }
                }
                None => {
                    rules.push(LearnedRule {
                        text: candidate.text.clone(),
                        scope: candidate.scope.clone(),
                        severity: candidate.severity,
                        category: candidate.category.clone(),
                        first_seen: seen.clone(),
                        last_seen: seen.clone(),
                        hits: 1,
                        example: candidate.example.clone(),
                    });
                    added.push(candidate.clone());
                }
            }
        }
        let rules = cap(rules, max);
        let kept: HashSet<_> = rules.iter().map(|r| rule_key(&r.text, r.scope.as_deref())).collect();
        added.retain(|c| kept.contains(&rule_key(&c.text, c.scope.as_deref())));
        MergeOutcome { book: Self { schema: RULES_SCHEMA.into(), rules: sorted(rules) }, added }
    }
}

fn cap(mut rules: Vec<LearnedRule>, max: usize) -> Vec<LearnedRule> {
    if rules.len() <= max {
        return rules;
    }
    rules.sort_by_key(|r| (Reverse(r.severity), Reverse(r.last_seen.at), Reverse(r.hits), r.text.clone()));
    rules.truncate(max);
    rules
}

fn sorted(mut rules: Vec<LearnedRule>) -> Vec<LearnedRule> {
    rules.sort_by_key(|r| {
        (Reverse(r.severity), Reverse(r.hits), Reverse(r.last_seen.at), r.text.to_lowercase(), r.scope.clone())
    });
    rules
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::handoff::domain::fixtures::failing_report;

    fn seen(commit: &str, day: u32) -> Sighting {
        Sighting { commit: commit.into(), at: Utc.with_ymd_and_hms(2026, 11, day, 9, 0, 0).unwrap() }
    }

    fn candidate(text: &str, scope: Option<&str>, severity: Severity) -> RuleCandidate {
        RuleCandidate {
            text: text.into(),
            scope: scope.map(str::to_string),
            severity,
            category: "correctness".into(),
            example: Some(RuleExample { location: "a.go:1".into(), evidence: "x".into() }),
        }
    }

    #[test]
    fn keys_ignore_case_whitespace_and_trailing_punctuation_but_not_scope() {
        assert_eq!(rule_key("Close  rows\nalways. ", None), rule_key("close rows always", None));
        assert_eq!(rule_key("Close rows!?", Some(" **/*.go ")), ("close rows".into(), "**/*.go".into()));
        assert_ne!(rule_key("Close rows", Some("**/*.go")), rule_key("Close rows", None));
    }

    #[test]
    fn candidates_come_from_shown_findings_with_rules_and_from_violations() {
        let report = failing_report();
        let rules = candidates(&report);
        let texts: Vec<&str> = rules.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Build SQL with bound parameters only.",
                "Name constants instead of magic numbers.",
                "R1 title",
                "R2 title"
            ]
        );
        assert_eq!(rules[0].severity, Severity::Critical);
        assert_eq!(rules[0].scope.as_deref(), Some("**/*.go"));
        assert_eq!(rules[0].example.as_ref().unwrap().location, "pay/db.go:42");
        assert!(rules[0].example.as_ref().unwrap().evidence.starts_with("query := "), "first evidence line only");
        assert_eq!((rules[2].category.as_str(), rules[2].severity), ("architecture", Severity::Major), "approved");
        assert_eq!((rules[3].category.as_str(), rules[3].severity), ("architecture", Severity::Minor), "inferred");
    }

    #[test]
    fn unaudited_llm_findings_are_not_promoted_to_rules() {
        let mut report = failing_report();
        report.audited = false;
        let texts: Vec<String> = candidates(&report).into_iter().map(|c| c.text).collect();
        assert_eq!(texts, vec!["R1 title", "R2 title"]);
    }

    #[test]
    fn audited_llm_findings_are_promoted_but_low_confidence_ones_are_not() {
        let mut report = failing_report();
        report.findings[1].confidence = Confidence::Low;
        let texts: Vec<String> = candidates(&report).into_iter().map(|c| c.text).collect();
        assert!(texts.contains(&"Build SQL with bound parameters only.".to_string()));
        assert!(!texts.contains(&"Name constants instead of magic numbers.".to_string()));
    }

    #[test]
    fn static_tool_findings_are_promoted_regardless_of_audit() {
        let mut report = failing_report();
        report.audited = false;
        report.findings[0].source = FindingSource::StaticTool("clippy".into());
        report.findings[0].confidence = Confidence::Low;
        let texts: Vec<String> = candidates(&report).into_iter().map(|c| c.text).collect();
        assert!(texts.contains(&"Build SQL with bound parameters only.".to_string()));
        assert!(!texts.contains(&"Name constants instead of magic numbers.".to_string()));
    }

    #[test]
    fn candidates_are_deduplicated_keeping_the_highest_severity() {
        let mut report = failing_report();
        let rule = report.findings[0].prevention_rule.clone();
        report.findings[1].prevention_rule = rule.map(|mut r| {
            r.text = r.text.to_uppercase();
            r
        });
        let rules = candidates(&report);
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].severity, Severity::Critical);
    }

    #[test]
    fn untitled_violations_name_the_rule() {
        let mut report = failing_report();
        let violation = &mut report.agents.as_mut().unwrap().violations[1];
        violation.rule_title = "  ".into();
        violation.file = String::new();
        let rule = candidates(&report).into_iter().find(|c| c.text.contains("R1")).unwrap();
        assert_eq!(
            (rule.text.as_str(), rule.severity, rule.example),
            ("Follow architecture rule R1", Severity::Major, None)
        );
    }

    #[test]
    fn long_evidence_is_cut() {
        let long = "x".repeat(EXAMPLE_CHARS + 10);
        let ex = example("a.go", 3, &format!("\n  {long}\nsecond")).unwrap();
        assert_eq!(ex.evidence.chars().count(), EXAMPLE_CHARS + 1);
        assert!(ex.evidence.ends_with('…'));
    }

    #[test]
    fn merge_adds_new_rules_and_counts_hits_once_per_commit() {
        let book = RuleBook::default();
        let first = book.merge(&[candidate("Close rows.", None, Severity::Minor)], &seen("aaaaaaa", 1), 100);
        assert_eq!(first.added.len(), 1);
        assert_eq!(first.book.rules[0].hits, 1);

        let again = first.book.merge(&[candidate("close  ROWS", None, Severity::Major)], &seen("aaaaaaa", 2), 100);
        assert!(again.added.is_empty(), "re-review of the same commit adds nothing");
        assert_eq!(again.book.rules[0].hits, 1);
        assert_eq!(again.book.rules[0].severity, Severity::Major, "severity only goes up");

        let later = again.book.merge(&[candidate("Close rows", None, Severity::Minor)], &seen("bbbbbbb", 3), 100);
        let rule = &later.book.rules[0];
        assert_eq!((rule.hits, rule.severity, rule.text.as_str()), (2, Severity::Major, "Close rows."));
        assert_eq!((rule.first_seen.commit.as_str(), rule.last_seen.commit.as_str()), ("aaaaaaa", "bbbbbbb"));
    }

    #[test]
    fn merge_keeps_the_example_when_the_candidate_has_none() {
        let book = RuleBook::default().merge(&[candidate("r", None, Severity::Minor)], &seen("a", 1), 10).book;
        let bare = RuleCandidate { example: None, ..candidate("r", None, Severity::Minor) };
        let merged = book.merge(&[bare], &seen("b", 2), 10).book;
        assert!(merged.rules[0].example.is_some());
    }

    #[test]
    fn merge_sorts_by_severity_then_hits() {
        let mut book = RuleBook::default();
        book = book.merge(&[candidate("often", None, Severity::Minor)], &seen("a", 1), 10).book;
        book = book.merge(&[candidate("often", None, Severity::Minor)], &seen("b", 2), 10).book;
        book = book
            .merge(
                &[candidate("rare", None, Severity::Minor), candidate("bad", Some("**/*.go"), Severity::Critical)],
                &seen("c", 3),
                10,
            )
            .book;
        let texts: Vec<&str> = book.rules.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["bad", "often", "rare"]);
    }

    #[test]
    fn cap_drops_least_severe_least_recent_first() {
        let mut book = RuleBook::default();
        book = book.merge(&[candidate("old minor", None, Severity::Minor)], &seen("a", 1), 3).book;
        book = book.merge(&[candidate("old critical", None, Severity::Critical)], &seen("b", 2), 3).book;
        book = book.merge(&[candidate("new minor", None, Severity::Minor)], &seen("c", 3), 3).book;
        let outcome = book.merge(
            &[candidate("newest info", None, Severity::Info), candidate("newer major", None, Severity::Major)],
            &seen("d", 4),
            3,
        );
        let texts: Vec<&str> = outcome.book.rules.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["old critical", "newer major", "new minor"]);
        let added: Vec<&str> = outcome.added.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(added, vec!["newer major"], "dropped rules are not reported as added");
    }

    #[test]
    fn json_round_trips_and_rejects_other_schemas() {
        let book =
            RuleBook::default().merge(&[candidate("r", Some("**/*.go"), Severity::Major)], &seen("a", 1), 10).book;
        assert_eq!(RuleBook::from_json(book.to_json().as_bytes()).unwrap(), book);
        assert!(
            RuleBook::from_json(b"{\"schema\":\"qgate.rules/v9\",\"rules\":[]}")
                .unwrap_err()
                .to_string()
                .contains("v9")
        );
        assert!(RuleBook::from_json(b"not json").is_err());
    }

    #[test]
    fn paths_put_the_state_next_to_the_markdown() {
        let paths = RulesPaths::new("shop-api", "qgate-rules.md");
        assert_eq!(paths.markdown, "shop-api/qgate-rules.md");
        assert_eq!(paths.state, "shop-api/.qgate-rules.json");
        assert_eq!(paths.file_names(), ["qgate-rules.md", ".qgate-rules.json"]);
    }
}
