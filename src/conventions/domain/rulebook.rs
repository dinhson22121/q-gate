use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_RULES: usize = 40;
pub const MAX_EXAMPLES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Explicit,
    Inferred,
    Team,
}

impl RuleKind {
    pub const ALL: [Self; 2] = [Self::Explicit, Self::Inferred];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Inferred => "inferred",
            Self::Team => "team",
        }
    }

    pub fn default_status(self) -> RuleStatus {
        match self {
            Self::Explicit => RuleStatus::Approved,
            Self::Inferred | Self::Team => RuleStatus::Proposed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    Proposed,
    Approved,
    Rejected,
}

impl RuleStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "proposed" => Some(Self::Proposed),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLine {
    pub file: String,
    pub line: u32,
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub title: String,
    pub description: String,
    pub kind: RuleKind,
    pub status: RuleStatus,
    pub scope_globs: Vec<String>,
    pub examples: Vec<FileLine>,
    pub lint_suggestion: Option<String>,
}

impl Rule {
    pub fn is_usable(&self, use_inferred_rules: bool) -> bool {
        match self.status {
            RuleStatus::Approved => true,
            RuleStatus::Proposed => use_inferred_rules,
            RuleStatus::Rejected => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDraft {
    pub title: String,
    pub description: String,
    pub kind: RuleKind,
    pub scope_globs: Vec<String>,
    pub examples: Vec<FileLine>,
    pub lint_suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRule {
    pub replaces: Option<RuleRef>,
    pub title: String,
    pub description: String,
    pub scope_globs: Vec<String>,
    pub status: RuleStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamRuleError {
    #[error("rule {0} changed or no longer exists; reload the rulebook and try again")]
    StaleReference(String),
    #[error("the rulebook already has the maximum of {MAX_RULES} rules")]
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rulebook {
    pub repo: String,
    pub fingerprint: String,
    pub rules: Vec<Rule>,
    pub created_at: DateTime<Utc>,
}

impl Rulebook {
    pub fn assemble(
        repo: String,
        fingerprint: String,
        drafts: Vec<RuleDraft>,
        decisions: &HashMap<String, RuleStatus>,
        created_at: DateTime<Utc>,
    ) -> Self {
        let mut seen = std::collections::HashSet::new();
        let rules = drafts
            .into_iter()
            .filter(|d| seen.insert(normalize_title(&d.title)))
            .take(MAX_RULES)
            .enumerate()
            .map(|(index, draft)| {
                let status =
                    decisions.get(&normalize_title(&draft.title)).copied().unwrap_or(draft.kind.default_status());
                Rule {
                    id: format!("R{}", index + 1),
                    title: draft.title,
                    description: draft.description,
                    kind: draft.kind,
                    status,
                    scope_globs: draft.scope_globs,
                    examples: draft.examples,
                    lint_suggestion: draft.lint_suggestion,
                }
            })
            .collect();
        Self { repo, fingerprint, rules, created_at }
    }

    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    pub fn with_status(&self, id: &str, status: RuleStatus) -> Option<Self> {
        self.rule(id)?;
        let rules = self.rules.iter().map(|r| if r.id == id { Rule { status, ..r.clone() } } else { r.clone() });
        Some(Self { rules: rules.collect(), ..self.clone() })
    }

    pub fn empty(repo: String, created_at: DateTime<Utc>) -> Self {
        Self { repo, fingerprint: String::new(), rules: Vec::new(), created_at }
    }

    pub fn team_drafts(&self) -> Vec<RuleDraft> {
        self.rules
            .iter()
            .filter(|r| r.kind == RuleKind::Team)
            .map(|r| RuleDraft {
                title: r.title.clone(),
                description: r.description.clone(),
                kind: RuleKind::Team,
                scope_globs: r.scope_globs.clone(),
                examples: Vec::new(),
                lint_suggestion: r.lint_suggestion.clone(),
            })
            .collect()
    }

    pub fn with_team_rules(&self, writes: &[TeamRule]) -> Result<(Self, Vec<(String, RuleStatus)>), TeamRuleError> {
        let mut rules = self.rules.clone();
        let mut decisions = Vec::new();
        let mut next_id = rules.iter().filter_map(|r| r.id.strip_prefix('R')?.parse::<usize>().ok()).max().unwrap_or(0);
        for write in writes {
            let key = normalize_title(&write.title);
            let mut slot = None;
            if let Some(target) = &write.replaces {
                let index = rules
                    .iter()
                    .position(|r| r.id == target.id && normalize_title(&r.title) == normalize_title(&target.title))
                    .ok_or_else(|| TeamRuleError::StaleReference(target.id.clone()))?;
                let old = rules.remove(index);
                let old_key = normalize_title(&old.title);
                if old_key != key {
                    decisions.push((old_key, RuleStatus::Rejected));
                }
                slot = Some((index, old.id));
            }
            let rule = |id: String| Rule {
                id,
                title: write.title.clone(),
                description: write.description.clone(),
                kind: RuleKind::Team,
                status: write.status,
                scope_globs: write.scope_globs.clone(),
                examples: Vec::new(),
                lint_suggestion: None,
            };
            if let Some(existing) = rules.iter_mut().find(|r| normalize_title(&r.title) == key) {
                *existing = rule(existing.id.clone());
            } else if let Some((index, id)) = slot {
                rules.insert(index, rule(id));
            } else {
                next_id += 1;
                rules.push(rule(format!("R{next_id}")));
            }
            decisions.push((key, write.status));
        }
        if rules.len() > MAX_RULES {
            return Err(TeamRuleError::Full);
        }
        Ok((Self { rules, ..self.clone() }, decisions))
    }

    pub fn is_stale(&self, current_fingerprint: &str, now: DateTime<Utc>, refresh_days: u32) -> bool {
        self.fingerprint != current_fingerprint || now - self.created_at >= Duration::days(i64::from(refresh_days))
    }
}

pub fn normalize_title(title: &str) -> String {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn draft(title: &str, kind: RuleKind) -> RuleDraft {
        RuleDraft {
            title: title.into(),
            description: format!("{title} description"),
            kind,
            scope_globs: Vec::new(),
            examples: vec![FileLine { file: "src/a.rs".into(), line: 1, quote: "fn main() {}".into() }],
            lint_suggestion: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::draft;
    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-11-{day:02}T00:00:00Z")).unwrap().into()
    }

    #[test]
    fn explicit_rules_start_approved_and_inferred_proposed() {
        let book = Rulebook::assemble(
            "r".into(),
            "fp".into(),
            vec![draft("Domain is pure", RuleKind::Explicit), draft("Use thiserror", RuleKind::Inferred)],
            &HashMap::new(),
            at(1),
        );
        assert_eq!(book.rules[0].id, "R1");
        assert_eq!(book.rules[0].status, RuleStatus::Approved);
        assert_eq!(book.rules[1].id, "R2");
        assert_eq!(book.rules[1].status, RuleStatus::Proposed);
    }

    #[test]
    fn decisions_carry_over_by_normalized_title_and_duplicates_collapse() {
        let decisions = HashMap::from([
            (normalize_title("use THISERROR!"), RuleStatus::Rejected),
            (normalize_title("domain is pure"), RuleStatus::Rejected),
        ]);
        let book = Rulebook::assemble(
            "r".into(),
            "fp".into(),
            vec![
                draft("Use thiserror", RuleKind::Inferred),
                draft("Use  thiserror.", RuleKind::Inferred),
                draft("Domain is pure", RuleKind::Explicit),
            ],
            &decisions,
            at(1),
        );
        assert_eq!(book.rules.len(), 2);
        assert!(book.rules.iter().all(|r| r.status == RuleStatus::Rejected));
    }

    #[test]
    fn caps_rule_count() {
        let drafts = (0..50).map(|i| draft(&format!("rule {i}"), RuleKind::Inferred)).collect();
        let book = Rulebook::assemble("r".into(), "fp".into(), drafts, &HashMap::new(), at(1));
        assert_eq!(book.rules.len(), MAX_RULES);
        assert_eq!(book.rules[39].id, "R40");
    }

    #[test]
    fn stale_when_fingerprint_changes_or_refresh_days_pass() {
        let book = Rulebook { repo: "r".into(), fingerprint: "a".into(), rules: Vec::new(), created_at: at(1) };
        assert!(!book.is_stale("a", at(7), 7));
        assert!(book.is_stale("a", at(8), 7));
        assert!(book.is_stale("b", at(1), 7));
    }

    #[test]
    fn usability_and_status_changes() {
        let book = Rulebook::assemble(
            "r".into(),
            "fp".into(),
            vec![draft("a", RuleKind::Explicit), draft("b", RuleKind::Inferred)],
            &HashMap::new(),
            at(1),
        );
        assert!(book.rules[0].is_usable(false));
        assert!(!book.rules[1].is_usable(false));
        assert!(book.rules[1].is_usable(true));
        let rejected = book.with_status("R1", RuleStatus::Rejected).unwrap();
        assert!(!rejected.rules[0].is_usable(true));
        assert_eq!(book.rules[0].status, RuleStatus::Approved);
        assert!(book.with_status("R9", RuleStatus::Approved).is_none());
    }

    #[test]
    fn status_and_kind_strings_round_trip() {
        for status in [RuleStatus::Proposed, RuleStatus::Approved, RuleStatus::Rejected] {
            assert_eq!(RuleStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(RuleStatus::parse("maybe"), None);
        assert_eq!(RuleKind::ALL.map(RuleKind::as_str), ["explicit", "inferred"]);
        assert_eq!(normalize_title("  Never -- log SECRETS!  "), "never log secrets");
    }

    fn team(title: &str, replaces: Option<(&str, &str)>, status: RuleStatus) -> TeamRule {
        TeamRule {
            replaces: replaces.map(|(id, title)| RuleRef { id: id.into(), title: title.into() }),
            title: title.into(),
            description: format!("{title}."),
            scope_globs: vec!["src/**".into()],
            status,
        }
    }

    fn book() -> Rulebook {
        Rulebook::assemble(
            "r".into(),
            "fp".into(),
            vec![draft("Domain is pure", RuleKind::Explicit), draft("Use thiserror", RuleKind::Inferred)],
            &HashMap::new(),
            at(1),
        )
    }

    #[test]
    fn team_rules_are_appended_edited_and_kept_as_drafts() {
        let (added, decisions) =
            book().with_team_rules(&[team("No unwrap in handlers", None, RuleStatus::Approved)]).unwrap();
        assert_eq!(added.rules[2].id, "R3");
        assert_eq!((added.rules[2].kind, added.rules[2].status), (RuleKind::Team, RuleStatus::Approved));
        assert_eq!(decisions, vec![("no unwrap in handlers".to_string(), RuleStatus::Approved)]);
        assert_eq!(RuleKind::Team.as_str(), "team");
        assert_eq!(RuleKind::Team.default_status(), RuleStatus::Proposed);

        let (edited, decisions) = added
            .with_team_rules(&[team("Errors use thiserror", Some(("R2", "Use thiserror")), RuleStatus::Proposed)])
            .unwrap();
        assert_eq!(edited.rules[1].id, "R2");
        assert_eq!(edited.rules[1].title, "Errors use thiserror");
        assert_eq!(
            decisions,
            vec![
                ("use thiserror".to_string(), RuleStatus::Rejected),
                ("errors use thiserror".to_string(), RuleStatus::Proposed)
            ]
        );
        let drafts = edited.team_drafts();
        assert_eq!(drafts.len(), 2);
        assert!(drafts.iter().all(|d| d.kind == RuleKind::Team && d.examples.is_empty()));
    }

    #[test]
    fn same_title_updates_in_place_and_keeping_the_title_is_not_a_rejection() {
        let (updated, decisions) = book()
            .with_team_rules(&[team("domain IS pure", Some(("R1", "Domain is pure")), RuleStatus::Approved)])
            .unwrap();
        assert_eq!(updated.rules.len(), 2);
        assert_eq!((updated.rules[0].id.as_str(), updated.rules[0].kind), ("R1", RuleKind::Team));
        assert_eq!(decisions, vec![("domain is pure".to_string(), RuleStatus::Approved)]);
        let (same, _) = book().with_team_rules(&[team("Use thiserror!", None, RuleStatus::Approved)]).unwrap();
        assert_eq!((same.rules.len(), same.rules[1].kind), (2, RuleKind::Team));
    }

    #[test]
    fn stale_references_and_full_rulebooks_are_refused() {
        let stale = book().with_team_rules(&[team("x", Some(("R1", "Another title")), RuleStatus::Approved)]);
        assert_eq!(stale.unwrap_err(), TeamRuleError::StaleReference("R1".into()));
        let drafts = (0..MAX_RULES).map(|i| draft(&format!("rule {i}"), RuleKind::Inferred)).collect();
        let full = Rulebook::assemble("r".into(), "fp".into(), drafts, &HashMap::new(), at(1));
        assert_eq!(
            full.with_team_rules(&[team("one more", None, RuleStatus::Approved)]).unwrap_err(),
            TeamRuleError::Full
        );
        let empty = Rulebook::empty("r".into(), at(2));
        assert!(empty.rules.is_empty() && empty.fingerprint.is_empty());
    }
}
