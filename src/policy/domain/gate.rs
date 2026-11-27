use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::review::application::{Category, Confidence, GatePolicy, Severity, UnverifiablePolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateField {
    FailOn,
    Categories,
    MinConfidence,
    FailOnUnmetCriteria,
    FailOnIncomplete,
    Unverifiable,
}

impl GateField {
    pub const ALL: [Self; 6] = [
        Self::FailOn,
        Self::Categories,
        Self::MinConfidence,
        Self::FailOnUnmetCriteria,
        Self::FailOnIncomplete,
        Self::Unverifiable,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailOn => "fail_on",
            Self::Categories => "categories",
            Self::MinConfidence => "min_confidence",
            Self::FailOnUnmetCriteria => "fail_on_unmet_criteria",
            Self::FailOnIncomplete => "fail_on_incomplete",
            Self::Unverifiable => "unverifiable",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == raw)
    }
}

fn to_value(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

pub fn gate_value(policy: &GatePolicy, field: GateField) -> Value {
    match field {
        GateField::FailOn => to_value(policy.fail_on),
        GateField::Categories => to_value(&policy.categories),
        GateField::MinConfidence => to_value(policy.min_confidence),
        GateField::FailOnUnmetCriteria => Value::Bool(policy.fail_on_unmet_criteria),
        GateField::FailOnIncomplete => Value::Bool(policy.fail_on_incomplete),
        GateField::Unverifiable => to_value(policy.unverifiable),
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value, hint: &str) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|_| format!("must be {hint}"))
}

pub fn with_gate_value(policy: &GatePolicy, field: GateField, value: &Value) -> Result<GatePolicy, String> {
    let mut next = policy.clone();
    match field {
        GateField::FailOn => next.fail_on = parse::<Severity>(value, "one of info, minor, major, critical")?,
        GateField::Categories => {
            let categories: Vec<Category> = parse(value, "a list of finding categories")?;
            if categories.is_empty() {
                return Err("must list at least one category".into());
            }
            next.categories = categories;
        }
        GateField::MinConfidence => next.min_confidence = parse::<Confidence>(value, "one of low, medium, high")?,
        GateField::FailOnUnmetCriteria => next.fail_on_unmet_criteria = parse(value, "true or false")?,
        GateField::FailOnIncomplete => next.fail_on_incomplete = parse(value, "true or false")?,
        GateField::Unverifiable => next.unverifiable = parse::<UnverifiablePolicy>(value, "one of strict, advisory")?,
    }
    Ok(next)
}

pub fn loosens_gate(field: GateField, before: &GatePolicy, after: &GatePolicy) -> bool {
    match field {
        GateField::FailOn => after.fail_on > before.fail_on,
        GateField::Categories => before.categories.iter().any(|c| !after.categories.contains(c)),
        GateField::MinConfidence => after.min_confidence > before.min_confidence,
        GateField::FailOnUnmetCriteria => before.fail_on_unmet_criteria && !after.fail_on_unmet_criteria,
        GateField::FailOnIncomplete => before.fail_on_incomplete && !after.fail_on_incomplete,
        GateField::Unverifiable => {
            before.unverifiable == UnverifiablePolicy::Strict && after.unverifiable == UnverifiablePolicy::Advisory
        }
    }
}

const BLOCK: &str = "block";
const CHECK_SWITCHES: &[&str] = &[
    "agents.enabled",
    "agents.tests.enabled",
    "agents.architecture.enabled",
    "agents.ticket_analyst.enabled",
    "figma.enabled",
    "jira.enabled",
    "watch.run_repo_checks",
];

pub fn loosens_setting(path: &str, old: &Value, new: &Value) -> bool {
    match path {
        "agents.tests.on_fail" | "agents.architecture.on_violation" => old == BLOCK && new != BLOCK,
        "figma.block_on" | "figma.checks" => removes_items(old, new),
        _ if CHECK_SWITCHES.contains(&path) => old == &Value::Bool(true) && new == &Value::Bool(false),
        _ => false,
    }
}

fn removes_items(old: &Value, new: &Value) -> bool {
    match (old.as_array(), new.as_array()) {
        (Some(old), Some(new)) => old.iter().any(|item| !new.contains(item)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_and_sets_gate_fields() {
        let policy = GatePolicy::default();
        assert_eq!(gate_value(&policy, GateField::FailOn), json!("critical"));
        assert_eq!(gate_value(&policy, GateField::FailOnIncomplete), json!(true));
        let stricter = with_gate_value(&policy, GateField::FailOn, &json!("major")).unwrap();
        assert_eq!(stricter.fail_on, Severity::Major);
        let cats = with_gate_value(&policy, GateField::Categories, &json!(["security"])).unwrap();
        assert_eq!(cats.categories, vec![Category::Security]);
        assert!(with_gate_value(&policy, GateField::Categories, &json!([])).is_err());
        assert!(with_gate_value(&policy, GateField::MinConfidence, &json!("sure")).unwrap_err().contains("low"));
        let lenient = with_gate_value(&policy, GateField::FailOnUnmetCriteria, &json!(false)).unwrap();
        assert!(!lenient.fail_on_unmet_criteria);
        assert!(with_gate_value(&policy, GateField::FailOnIncomplete, &json!("no")).is_err());
        assert_eq!(gate_value(&policy, GateField::Unverifiable), json!("strict"));
        let advisory = with_gate_value(&policy, GateField::Unverifiable, &json!("advisory")).unwrap();
        assert_eq!(advisory.unverifiable, UnverifiablePolicy::Advisory);
        assert!(with_gate_value(&policy, GateField::Unverifiable, &json!("lax")).unwrap_err().contains("strict"));
        assert_eq!(GateField::ALL.map(|f| GateField::parse(f.as_str())), GateField::ALL.map(Some));
        assert_eq!(GateField::parse("nope"), None);
    }

    #[test]
    fn detects_gate_loosening() {
        let before = GatePolicy::default();
        let set = |field, value: Value| with_gate_value(&before, field, &value).unwrap();
        assert!(!loosens_gate(GateField::FailOn, &before, &set(GateField::FailOn, json!("major"))));
        let raised = GatePolicy { fail_on: Severity::Info, ..before.clone() };
        assert!(loosens_gate(GateField::FailOn, &raised, &before));
        assert!(loosens_gate(GateField::Categories, &before, &set(GateField::Categories, json!(["security"]))));
        let unsure = GatePolicy { min_confidence: Confidence::Low, ..before.clone() };
        assert!(loosens_gate(GateField::MinConfidence, &unsure, &before));
        let off = |f| set(f, json!(false));
        assert!(loosens_gate(GateField::FailOnUnmetCriteria, &before, &off(GateField::FailOnUnmetCriteria)));
        assert!(loosens_gate(GateField::FailOnIncomplete, &before, &off(GateField::FailOnIncomplete)));
        assert!(!loosens_gate(GateField::FailOnIncomplete, &off(GateField::FailOnIncomplete), &before));
        let advisory = set(GateField::Unverifiable, json!("advisory"));
        assert!(loosens_gate(GateField::Unverifiable, &before, &advisory));
        assert!(!loosens_gate(GateField::Unverifiable, &advisory, &before));
    }

    #[test]
    fn detects_setting_loosening() {
        assert!(loosens_setting("agents.tests.on_fail", &json!("block"), &json!("warn")));
        assert!(!loosens_setting("agents.tests.on_fail", &json!("warn"), &json!("block")));
        assert!(loosens_setting("figma.block_on", &json!(["copy", "token"]), &json!(["copy"])));
        assert!(!loosens_setting("figma.block_on", &json!(["copy"]), &json!(["copy", "token"])));
        assert!(loosens_setting("agents.architecture.enabled", &json!(true), &json!(false)));
        assert!(!loosens_setting("figma.enabled", &json!(false), &json!(true)));
        assert!(!loosens_setting("agents.model", &json!("a"), &json!("b")));
        assert!(!loosens_setting("figma.checks", &json!("x"), &json!([])));
    }
}
