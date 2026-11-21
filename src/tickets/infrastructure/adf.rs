//! Atlassian Document Format (Jira Cloud rich text) → plain text that
//! [`parse_acceptance_criteria`](crate::tickets::domain::parse_acceptance_criteria) understands:
//! headings become `# `, list items `- `, task items `- [ ]` / `- [x]`.

use serde_json::Value;

/// ADF `taskItem` state meaning "checked".
const TASK_DONE: &str = "DONE";
/// Marker for whole-paragraph bold text, so bold lines can act as headings.
const BOLD: &str = "**";
/// Highest heading level ADF allows.
const MAX_HEADING_LEVEL: u64 = 6;

/// Converts an ADF document (or any ADF node) to plain text, one block per line.
pub fn adf_to_text(node: &Value) -> String {
    let mut lines = Vec::new();
    render_block(node, &mut lines);
    lines.join("\n").trim().to_string()
}

/// Converts a field that may be an ADF document, a plain string, a list of those, or null.
/// Returns `None` when there is no content.
pub fn field_to_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) => s.trim().to_string(),
        Value::Array(items) => items.iter().filter_map(field_to_text).collect::<Vec<_>>().join("\n"),
        Value::Object(_) => adf_to_text(value),
        _ => String::new(),
    };
    (!text.is_empty()).then_some(text)
}

/// Every string inside `value` (text, link hrefs, card URLs...), one per line. Used to find
/// links that the plain-text rendering drops, such as `link` mark hrefs.
pub fn all_strings(value: &Value) -> String {
    let mut out = Vec::new();
    collect_strings(value, &mut out);
    out.join("\n")
}

fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        Value::Object(map) => map.values().for_each(|item| collect_strings(item, out)),
        _ => {}
    }
}

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn children(node: &Value) -> &[Value] {
    node.get("content").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn render_block(node: &Value, lines: &mut Vec<String>) {
    match node_type(node) {
        "paragraph" => push_inline(node, "", lines),
        "heading" => push_inline(node, &heading_prefix(node), lines),
        "listItem" => render_list_item(node, "- ", lines),
        "taskItem" => push_inline(node, task_prefix(node), lines),
        "codeBlock" => lines.extend(inline_text(node).lines().map(str::to_string)),
        "rule" => lines.push(String::new()),
        "text" | "hardBreak" | "mention" | "emoji" | "inlineCard" | "date" | "status" => push_inline_node(node, lines),
        _ => children(node).iter().for_each(|child| render_block(child, lines)),
    }
}

fn heading_prefix(node: &Value) -> String {
    let level = node.pointer("/attrs/level").and_then(Value::as_u64).unwrap_or(1).clamp(1, MAX_HEADING_LEVEL);
    format!("{} ", "#".repeat(level as usize))
}

fn task_prefix(node: &Value) -> &'static str {
    match node.pointer("/attrs/state").and_then(Value::as_str) {
        Some(TASK_DONE) => "- [x] ",
        _ => "- [ ] ",
    }
}

/// The first paragraph of a list item carries the bullet; nested lists and later blocks follow.
fn render_list_item(node: &Value, prefix: &str, lines: &mut Vec<String>) {
    let mut has_bullet = false;
    for child in children(node) {
        if !has_bullet && node_type(child) == "paragraph" {
            push_inline(child, prefix, lines);
            has_bullet = true;
        } else {
            render_block(child, lines);
        }
    }
}

/// Renders a block's inline content as lines (hard breaks split lines); the prefix goes on the first.
fn push_inline(node: &Value, prefix: &str, lines: &mut Vec<String>) {
    let mut text = inline_text(node);
    if node_type(node) == "paragraph" && is_all_bold(node) {
        text = format!("{BOLD}{}{BOLD}", text.trim());
    }
    let mut parts = text.split('\n');
    lines.push(format!("{prefix}{}", parts.next().unwrap_or_default()));
    lines.extend(parts.map(str::to_string));
}

fn push_inline_node(node: &Value, lines: &mut Vec<String>) {
    let mut text = String::new();
    append_inline(node, &mut text);
    lines.extend(text.split('\n').map(str::to_string));
}

fn inline_text(node: &Value) -> String {
    let mut text = String::new();
    children(node).iter().for_each(|child| append_inline(child, &mut text));
    text
}

fn append_inline(node: &Value, out: &mut String) {
    let attr = |name: &str| node.pointer(&format!("/attrs/{name}")).and_then(Value::as_str);
    match node_type(node) {
        "text" => out.push_str(node.get("text").and_then(Value::as_str).unwrap_or_default()),
        "hardBreak" => out.push('\n'),
        "mention" | "emoji" => out.push_str(attr("text").or_else(|| attr("shortName")).unwrap_or_default()),
        "inlineCard" => out.push_str(attr("url").unwrap_or_default()),
        "status" => out.push_str(attr("text").unwrap_or_default()),
        "date" => out.push_str(attr("timestamp").unwrap_or_default()),
        _ => children(node).iter().for_each(|child| append_inline(child, out)),
    }
}

/// Whether every non-blank text node of a paragraph carries the `strong` mark.
fn is_all_bold(node: &Value) -> bool {
    let texts: Vec<&Value> = children(node)
        .iter()
        .filter(|c| node_type(c) == "text")
        .filter(|c| c.get("text").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty()))
        .collect();
    !texts.is_empty()
        && texts.len() == children(node).iter().filter(|c| node_type(c) != "hardBreak").count()
        && texts.iter().all(|t| has_mark(t, "strong"))
}

fn has_mark(node: &Value, mark: &str) -> bool {
    node.get("marks").and_then(Value::as_array).is_some_and(|marks| marks.iter().any(|m| node_type(m) == mark))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tickets::domain::parse_acceptance_criteria;

    fn text(t: &str) -> Value {
        json!({"type": "text", "text": t})
    }

    fn para(content: Vec<Value>) -> Value {
        json!({"type": "paragraph", "content": content})
    }

    fn item(t: &str) -> Value {
        json!({"type": "listItem", "content": [para(vec![text(t)])]})
    }

    fn doc(content: Vec<Value>) -> Value {
        json!({"type": "doc", "version": 1, "content": content})
    }

    #[test]
    fn converts_common_blocks() {
        let adf = doc(vec![
            json!({"type": "heading", "attrs": {"level": 2}, "content": [text("Acceptance Criteria")]}),
            json!({"type": "bulletList", "content": [item("First"), {
                "type": "listItem",
                "content": [para(vec![text("Second")]), {"type": "orderedList", "content": [item("Nested")]}]
            }]}),
            json!({"type": "taskList", "content": [
                {"type": "taskItem", "attrs": {"state": "TODO"}, "content": [text("Todo task")]},
                {"type": "taskItem", "attrs": {"state": "DONE"}, "content": [text("Done task")]}
            ]}),
            json!({"type": "codeBlock", "content": [text("let a = 1;\nlet b = 2;")]}),
            para(vec![text("line one"), json!({"type": "hardBreak"}), text("line two")]),
            json!({"type": "rule"}),
            para(vec![
                text("hi "),
                json!({"type": "mention", "attrs": {"text": "@Ann"}}),
                text(" "),
                json!({"type": "emoji", "attrs": {"shortName": ":smile:"}}),
            ]),
            para(vec![
                json!({"type": "inlineCard", "attrs": {"url": "https://x.test"}}),
                json!({"type": "status", "attrs": {"text": "DONE"}}),
            ]),
        ]);
        assert_eq!(
            adf_to_text(&adf),
            "## Acceptance Criteria\n- First\n- Second\n- Nested\n- [ ] Todo task\n- [x] Done task\nlet a = 1;\nlet b = 2;\nline one\nline two\n\nhi @Ann :smile:\nhttps://x.testDONE"
        );
    }

    #[test]
    fn bold_paragraph_is_wrapped_and_parsable_as_heading() {
        let strong = json!({"type": "text", "text": "Acceptance criteria:", "marks": [{"type": "strong"}]});
        let adf = doc(vec![
            para(vec![text("Context paragraph")]),
            para(vec![strong]),
            json!({"type": "bulletList", "content": [item("Can export"), item("Is audited")]}),
        ]);
        let plain = adf_to_text(&adf);
        assert!(plain.contains("**Acceptance criteria:**"), "{plain}");
        assert_eq!(parse_acceptance_criteria(&plain), vec!["Can export", "Is audited"]);
    }

    #[test]
    fn mixed_marks_are_not_bold() {
        let adf = para(vec![json!({"type": "text", "text": "a", "marks": [{"type": "strong"}]}), text("b")]);
        assert_eq!(adf_to_text(&adf), "ab");
    }

    #[test]
    fn tolerates_unknown_and_malformed_nodes() {
        let adf = doc(vec![
            json!({"type": "panel", "content": [para(vec![text("inside panel")])]}),
            json!({"type": "table", "content": [{"type": "tableRow", "content": [{"type": "tableCell", "content": [para(vec![text("cell")])]}]}]}),
            json!({"type": "mediaSingle"}),
            json!(42),
        ]);
        assert_eq!(adf_to_text(&adf), "inside panel\ncell");
        assert_eq!(adf_to_text(&json!({"type": "text", "text": "bare"})), "bare");
    }

    #[test]
    fn all_strings_includes_link_marks() {
        let link =
            json!({"type": "text", "text": "Design", "marks": [{"type": "link", "attrs": {"href": "https://h/x"}}]});
        let strings = all_strings(&doc(vec![para(vec![link]), json!(1)]));
        assert!(strings.contains("Design") && strings.contains("https://h/x"), "{strings}");
    }

    #[test]
    fn field_to_text_handles_all_shapes() {
        assert_eq!(field_to_text(&json!(null)), None);
        assert_eq!(field_to_text(&json!("  ")), None);
        assert_eq!(field_to_text(&json!(" - a ")).as_deref(), Some("- a"));
        assert_eq!(field_to_text(&json!(["a", null, "b"])).as_deref(), Some("a\nb"));
        assert_eq!(field_to_text(&doc(vec![para(vec![text("x")])])).as_deref(), Some("x"));
        assert_eq!(field_to_text(&doc(vec![])), None);
        assert_eq!(field_to_text(&json!(3)), None);
    }
}
