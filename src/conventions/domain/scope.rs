//! Which rules apply to a change.

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::rulebook::Rule;

fn globset(patterns: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
            any = true;
        }
    }
    if !any {
        return None;
    }
    builder.build().ok()
}

/// Whether the rule's scope covers any of `paths`. No (valid) globs means the rule applies everywhere.
pub fn rule_applies(rule: &Rule, paths: &[&str]) -> bool {
    match globset(&rule.scope_globs) {
        None => true,
        Some(set) => paths.iter().any(|p| set.is_match(p)),
    }
}

/// Usable rules (see [`Rule::is_usable`]) whose scope matches a changed path, in rulebook order.
pub fn select_rules<'a>(rules: &'a [Rule], paths: &[&str], use_inferred_rules: bool) -> Vec<&'a Rule> {
    rules.iter().filter(|r| r.is_usable(use_inferred_rules) && rule_applies(r, paths)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conventions::domain::rulebook::{RuleKind, RuleStatus};

    fn rule(id: &str, status: RuleStatus, globs: &[&str]) -> Rule {
        Rule {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            kind: RuleKind::Inferred,
            status,
            scope_globs: globs.iter().map(|s| s.to_string()).collect(),
            examples: Vec::new(),
            lint_suggestion: None,
        }
    }

    #[test]
    fn selects_by_scope_and_status() {
        let rules = vec![
            rule("R1", RuleStatus::Approved, &["src/domain/**"]),
            rule("R2", RuleStatus::Approved, &[]),
            rule("R3", RuleStatus::Proposed, &["**/*.rs"]),
            rule("R4", RuleStatus::Rejected, &[]),
            rule("R5", RuleStatus::Approved, &["web/**/*.ts", "["]),
            rule("R6", RuleStatus::Approved, &["["]),
        ];
        let ids = |selected: Vec<&Rule>| selected.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(select_rules(&rules, &["src/domain/user.rs"], true)), ["R1", "R2", "R3", "R6"]);
        assert_eq!(ids(select_rules(&rules, &["src/domain/user.rs"], false)), ["R1", "R2", "R6"]);
        assert_eq!(ids(select_rules(&rules, &["web/app/x.ts"], false)), ["R2", "R5", "R6"]);
    }
}
