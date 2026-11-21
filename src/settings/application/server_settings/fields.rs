use std::collections::BTreeMap;

use serde_json::Value;

use super::ports::SettingsError;
use crate::settings::domain::ServerConfig;

pub(super) fn to_json(config: &ServerConfig) -> Result<Value, SettingsError> {
    serde_json::to_value(config).map_err(|e| SettingsError::Storage(format!("cannot serialize settings: {e}")))
}

pub(super) fn from_json(value: Value) -> Result<ServerConfig, SettingsError> {
    serde_json::from_value(value).map_err(|e| SettingsError::Storage(format!("cannot read settings: {e}")))
}

fn pointer(path: &str) -> String {
    format!("/{}", path.replace('.', "/"))
}

pub(super) fn get<'a>(config: &'a Value, path: &str) -> Option<&'a Value> {
    config.pointer(&pointer(path))
}

pub(super) fn copy(source: &Value, target: &mut Value, path: &str) {
    if let (Some(value), Some(slot)) = (get(source, path), target.pointer_mut(&pointer(path))) {
        *slot = value.clone();
    }
}

pub(super) fn changed_paths(a: &Value, b: &Value) -> Vec<String> {
    let (left, right) = (leaves(a), leaves(b));
    let mut paths: Vec<String> =
        left.keys().chain(right.keys()).filter(|path| left.get(*path) != right.get(*path)).cloned().collect();
    paths.sort();
    paths.dedup();
    paths
}

fn leaves(value: &Value) -> BTreeMap<String, &Value> {
    let mut out = BTreeMap::new();
    collect_leaves(value, String::new(), &mut out);
    out
}

fn collect_leaves<'a>(value: &'a Value, prefix: String, out: &mut BTreeMap<String, &'a Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                collect_leaves(child, path, out);
            }
        }
        leaf => {
            out.insert(prefix, leaf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_copies_and_diffs_by_dotted_path() {
        let a = to_json(&ServerConfig::default()).unwrap();
        let mut changed = ServerConfig::default();
        changed.jira.email = "x@y.z".into();
        changed.figma.block_on = vec![crate::settings::domain::DesignCheck::Copy];
        let mut b = to_json(&changed).unwrap();
        assert_eq!(get(&b, "jira.email"), Some(&Value::from("x@y.z")));
        assert_eq!(changed_paths(&a, &b), vec!["figma.block_on".to_string(), "jira.email".to_string()]);
        copy(&a, &mut b, "jira.email");
        assert_eq!(changed_paths(&a, &b), vec!["figma.block_on".to_string()]);
        assert_eq!(from_json(b).unwrap().figma.block_on.len(), 1);
        assert!(from_json(Value::from(1)).is_err());
    }
}
