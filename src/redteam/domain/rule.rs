use serde::{Deserialize, Serialize};

use super::category::AttackCategory;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreventionRule {
    pub text: String,
    pub scope: Option<String>,
}

pub fn prevention_rule(category: AttackCategory, fix: &str) -> PreventionRule {
    PreventionRule { text: format!("Prevent {}: {fix}", category.label()), scope: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_names_the_category_and_keeps_the_fix() {
        let rule = prevention_rule(AttackCategory::SqlInjection, "use parameterized queries");
        assert_eq!(rule.text, "Prevent SQL injection: use parameterized queries");
        assert_eq!(rule.scope, None);
    }
}
