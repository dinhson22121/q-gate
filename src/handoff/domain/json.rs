//! JSON rendering (`qgate.review/v1`) of a [`HandoffDocument`] plus the full report, for tools.

use serde_json::{Value, json};

use super::document::{HandoffDocument, HandoffInput};

pub const JSON_SCHEMA: &str = "qgate.review/v1";

pub fn render_json(doc: &HandoffDocument, input: &HandoffInput<'_>) -> String {
    let value = json!({
        "schema": JSON_SCHEMA,
        "meta": doc.meta,
        "verdict": doc.verdict,
        "blocking": doc.blocking,
        "warnings": doc.warnings,
        "criteria": doc.criteria,
        "context": doc.context,
        "rules_to_add": doc.rules_to_add,
        "rules_file": doc.rules_file,
        "report": input.report,
    });
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| Value::Null.to_string());
    text.push('\n');
    text
}
