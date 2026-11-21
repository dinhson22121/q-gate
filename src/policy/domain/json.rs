//! Dotted-path access to a JSON document (`agents.tests.on_fail`). Settings are handled as JSON here so the
//! policy domain does not depend on the settings types; the settings context re-validates every change.

use serde_json::Value;

fn pointer(path: &str) -> String {
    format!("/{}", path.replace('.', "/"))
}

pub fn get<'a>(document: &'a Value, path: &str) -> Option<&'a Value> {
    document.pointer(&pointer(path))
}

/// Replaces the value at an existing `path`; returns `false` (and changes nothing) when it does not exist.
pub fn set(document: &mut Value, path: &str, value: Value) -> bool {
    match document.pointer_mut(&pointer(path)) {
        Some(slot) => {
            *slot = value;
            true
        }
        None => false,
    }
}

/// Dotted paths of every leaf (arrays count as leaves), in document order.
pub fn leaf_paths(document: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect(document, String::new(), &mut out);
    out
}

fn collect(value: &Value, prefix: String, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                collect(child, path, out);
            }
        }
        _ => out.push(prefix),
    }
}

/// Same JSON type (bool, number, string, array, object, null) — a change must not alter a field's shape.
pub fn same_kind(a: &Value, b: &Value) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_sets_and_lists_leaves_by_dotted_path() {
        let mut doc = json!({"a": {"b": 1, "c": [1, 2]}, "d": "x"});
        assert_eq!(get(&doc, "a.b"), Some(&json!(1)));
        assert!(set(&mut doc, "a.c", json!([3])));
        assert!(!set(&mut doc, "a.missing", json!(1)));
        assert_eq!(get(&doc, "a.c"), Some(&json!([3])));
        assert_eq!(leaf_paths(&doc), vec!["a.b", "a.c", "d"]);
        assert!(same_kind(&json!(1), &json!(2.5)));
        assert!(!same_kind(&json!("1"), &json!(1)));
    }
}
