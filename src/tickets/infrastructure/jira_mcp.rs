use std::process::Stdio;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::Value;

use crate::tickets::application::{TicketError, TicketSource};
use crate::tickets::domain::{
    ParentRef, Ticket, TicketKey, extract_figma_links, parse_acceptance_criteria, parse_criteria_list,
};

use super::adf::{all_strings, field_to_text};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const ISSUE_KEY_ARGUMENT: &str = "issue_key";
const INHERITED_ENV: &[&str] = &["PATH", "HOME", "USERPROFILE", "SYSTEMROOT", "TMPDIR", "TEMP", "TMP", "LANG"];
const NOT_FOUND_MARKERS: &[&str] = &["not found", "does not exist", "404"];
const UNAUTHORIZED_MARKERS: &[&str] = &["unauthorized", "forbidden", "401", "403", "permission"];

#[derive(Clone)]
pub struct JiraMcpSource {
    command: Vec<String>,
    tool: String,
    env: Vec<(String, String)>,
    timeout: Duration,
}

impl std::fmt::Debug for JiraMcpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env_names: Vec<&str> = self.env.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("JiraMcpSource")
            .field("command", &self.command)
            .field("tool", &self.tool)
            .field("env", &env_names)
            .finish()
    }
}

impl JiraMcpSource {
    pub fn new(command: Vec<String>, tool: String, env: Vec<(String, String)>) -> Result<Self, TicketError> {
        if command.first().is_none_or(|program| program.trim().is_empty()) {
            return Err(TicketError::Config("Jira MCP command is empty".into()));
        }
        if tool.trim().is_empty() {
            return Err(TicketError::Config("Jira MCP tool name is empty".into()));
        }
        Ok(Self { command, tool: tool.trim().to_string(), env, timeout: CALL_TIMEOUT })
    }

    fn spawn(&self) -> Result<TokioChildProcess, TicketError> {
        let (program, args) =
            self.command.split_first().ok_or_else(|| TicketError::Config("Jira MCP command is empty".into()))?;
        let inherited: Vec<(&str, String)> =
            INHERITED_ENV.iter().filter_map(|name| std::env::var(name).ok().map(|value| (*name, value))).collect();
        let command = tokio::process::Command::new(program).configure(|cmd| {
            cmd.args(args).env_clear().envs(inherited).envs(self.env.iter().cloned()).kill_on_drop(true);
        });
        let (transport, _stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| TicketError::Config(format!("cannot start Jira MCP server {program:?}: {e}")))?;
        Ok(transport)
    }

    async fn call(&self, key: &TicketKey) -> Result<CallToolResult, TicketError> {
        let transport = self.spawn()?;
        let client =
            ().serve(transport).await.map_err(|e| TicketError::Unavailable(format!("MCP initialize failed: {e}")))?;
        let mut arguments = JsonObject::new();
        arguments.insert(ISSUE_KEY_ARGUMENT.into(), Value::String(key.to_string()));
        let result = client.call_tool(CallToolRequestParams::new(self.tool.clone()).with_arguments(arguments)).await;
        let _ = client.cancel().await;
        result.map_err(|e| TicketError::Unavailable(format!("MCP tool {:?} failed: {e}", self.tool)))
    }
}

impl TicketSource for JiraMcpSource {
    async fn fetch(&self, key: &TicketKey) -> Result<Ticket, TicketError> {
        let result = tokio::time::timeout(self.timeout, self.call(key)).await.map_err(|_| {
            TicketError::Unavailable(format!("Jira MCP server did not answer within {}s", self.timeout.as_secs()))
        })??;
        map_tool_result(key, &result)
    }
}

pub fn map_tool_result(key: &TicketKey, result: &CallToolResult) -> Result<Ticket, TicketError> {
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    if result.is_error == Some(true) {
        return Err(classify_tool_error(key, &text));
    }
    if let Some(structured) = result.structured_content.as_ref().filter(|v| v.is_object()) {
        return Ok(ticket_from_json(key, structured));
    }
    if text.trim().is_empty() {
        return Err(TicketError::Invalid("MCP tool returned no text content".into()));
    }
    Ok(ticket_from_text(key, &text))
}

fn classify_tool_error(key: &TicketKey, text: &str) -> TicketError {
    let lower = text.to_lowercase();
    let summary = text.lines().next().unwrap_or("MCP tool reported an error").to_string();
    if NOT_FOUND_MARKERS.iter().any(|m| lower.contains(m)) {
        TicketError::NotFound(key.to_string())
    } else if UNAUTHORIZED_MARKERS.iter().any(|m| lower.contains(m)) {
        TicketError::Unauthorized(summary)
    } else {
        TicketError::Unavailable(summary)
    }
}

pub fn ticket_from_text(key: &TicketKey, text: &str) -> Ticket {
    match serde_json::from_str::<Value>(text.trim()) {
        Ok(json) if json.is_object() => ticket_from_json(key, &json),
        _ => ticket_from_markdown(key, text),
    }
}

pub fn ticket_from_json(key: &TicketKey, json: &Value) -> Ticket {
    let issue = json.get("issue").filter(|v| v.is_object()).unwrap_or(json);
    let lookup = |name: &str| issue.get(name).or_else(|| issue.pointer(&format!("/fields/{name}")));
    let string =
        |name: &str| lookup(name).and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let status = lookup("status").and_then(name_of);
    let from_ac_field = acceptance_field(issue)
        .and_then(field_to_text)
        .map(|text| parse_criteria_list(&text))
        .filter(|criteria| !criteria.is_empty());
    let raw_description = lookup("description").unwrap_or(&Value::Null);
    let description = field_to_text(raw_description).unwrap_or_default();
    let criteria = from_ac_field.unwrap_or_else(|| parse_acceptance_criteria(&description));
    Ticket {
        url: string("url"),
        status,
        criteria,
        issue_type: lookup("issue_type").or_else(|| lookup("issuetype")).and_then(name_of),
        parent: lookup("parent").and_then(parent_ref),
        design_links: extract_figma_links(&all_strings(raw_description)),
        description,
        updated: string("updated"),
        ..Ticket::new(key.clone(), string("summary").or_else(|| string("title")).unwrap_or_default())
    }
}

fn name_of(value: &Value) -> Option<String> {
    value.as_str().or_else(|| value.get("name").and_then(Value::as_str)).map(str::to_string)
}

fn parent_ref(value: &Value) -> Option<ParentRef> {
    let key = value.as_str().or_else(|| value.get("key").and_then(Value::as_str))?;
    let issue_type = ["/issue_type", "/issuetype", "/fields/issuetype"]
        .iter()
        .find_map(|pointer| value.pointer(pointer))
        .and_then(name_of);
    Some(ParentRef { key: TicketKey::parse(key).ok()?, issue_type })
}

fn acceptance_field(issue: &Value) -> Option<&Value> {
    field_named_acceptance(issue).or_else(|| issue.get("fields").and_then(field_named_acceptance))
}

fn field_named_acceptance(object: &Value) -> Option<&Value> {
    object
        .as_object()?
        .iter()
        .find(|(name, value)| name.to_lowercase().contains("acceptance") && !value.is_null())
        .map(|(_, value)| value)
}

fn ticket_from_markdown(key: &TicketKey, text: &str) -> Ticket {
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    let title = lines.next().unwrap_or_default().trim().trim_start_matches('#').trim().to_string();
    let description = lines.collect::<Vec<&str>>().join("\n").trim().to_string();
    Ticket {
        criteria: parse_acceptance_criteria(&description),
        design_links: extract_figma_links(&description),
        description,
        ..Ticket::new(key.clone(), title)
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::ContentBlock;
    use serde_json::json;

    use super::*;

    fn key() -> TicketKey {
        TicketKey::parse("ABC-1").unwrap()
    }

    fn text_result(text: &str, is_error: bool) -> CallToolResult {
        if is_error {
            CallToolResult::error(vec![ContentBlock::text(text)])
        } else {
            CallToolResult::success(vec![ContentBlock::text(text)])
        }
    }

    #[test]
    fn maps_mcp_atlassian_style_json() {
        let body = json!({
            "key": "ABC-1", "summary": "Export CSV", "url": "https://acme.atlassian.net/browse/ABC-1",
            "status": {"name": "To Do", "category": "To Do"},
            "description": "Intro\n\n## Acceptance Criteria\n- One\n- Two"
        });
        let ticket = map_tool_result(&key(), &text_result(&body.to_string(), false)).unwrap();
        assert_eq!(ticket.title, "Export CSV");
        assert_eq!(ticket.status.as_deref(), Some("To Do"));
        assert_eq!(ticket.url.as_deref(), Some("https://acme.atlassian.net/browse/ABC-1"));
        assert_eq!(ticket.criteria, vec!["One", "Two"]);
    }

    #[test]
    fn maps_nested_fields_and_acceptance_field() {
        let body = json!({"issue": {"fields": {
            "summary": "S", "status": "Done", "description": "## AC\n- from description",
            "Acceptance Criteria": "- from field"
        }}});
        let ticket = ticket_from_text(&key(), &body.to_string());
        assert_eq!(ticket.status.as_deref(), Some("Done"));
        assert_eq!(ticket.criteria, vec!["from field"]);
        let titled = ticket_from_json(&key(), &json!({"title": "T", "description": null}));
        assert_eq!(titled.title, "T");
        assert!(titled.criteria.is_empty());
    }

    #[test]
    fn maps_hierarchy_links_and_updated_when_present() {
        let body = json!({
            "summary": "Sub", "issue_type": {"name": "Sub-task"}, "updated": "2026-11-20",
            "parent": {"key": "ABC-0x", "issue_type": "Story"},
            "description": "See https://www.figma.com/design/K1/x?node-id=1-2"
        });
        let ticket = ticket_from_json(&key(), &body);
        assert_eq!(ticket.issue_type.as_deref(), Some("Sub-task"));
        assert_eq!(ticket.parent, None, "invalid parent key is ignored");
        assert_eq!(ticket.updated.as_deref(), Some("2026-11-20"));
        assert_eq!(ticket.design_links, vec!["https://www.figma.com/design/K1/x?node-id=1-2"]);

        let nested = json!({"fields": {"issuetype": "Task", "parent": {"key": "ABC-2", "fields": {"issuetype": {"name": "Story"}}}}});
        let parent = ticket_from_json(&key(), &nested).parent.unwrap();
        assert_eq!((parent.key.as_str(), parent.issue_type.as_deref()), ("ABC-2", Some("Story")));
        assert_eq!(ticket_from_json(&key(), &json!({"parent": "ABC-3"})).parent.unwrap().issue_type, None);
        assert_eq!(ticket_from_json(&key(), &json!({})).issue_type, None);
    }

    #[test]
    fn prefers_structured_content() {
        let mut result = text_result("ignored", false);
        result.structured_content = Some(json!({"summary": "Structured", "description": "AC:\n- x"}));
        let ticket = map_tool_result(&key(), &result).unwrap();
        assert_eq!((ticket.title.as_str(), ticket.criteria.clone()), ("Structured", vec!["x".to_string()]));
    }

    #[test]
    fn maps_markdown_text() {
        let text = "\n# ABC-1: Export CSV\n\nSome context.\n\n### Acceptance Criteria\n1. One\n2. Two\n";
        let ticket = map_tool_result(&key(), &text_result(text, false)).unwrap();
        assert_eq!(ticket.title, "ABC-1: Export CSV");
        assert_eq!(ticket.criteria, vec!["One", "Two"]);
        assert_eq!(ticket.url, None);
    }

    #[test]
    fn maps_tool_errors_and_empty_results() {
        let not_found = map_tool_result(&key(), &text_result("Issue does not exist or you lack access", true));
        assert_eq!(not_found, Err(TicketError::NotFound("ABC-1".into())));
        let unauthorized = map_tool_result(&key(), &text_result("HTTP 401 Unauthorized", true));
        assert!(matches!(unauthorized, Err(TicketError::Unauthorized(_))));
        let other = map_tool_result(&key(), &text_result("boom\ntrace", true));
        assert_eq!(other, Err(TicketError::Unavailable("boom".into())));
        let empty = map_tool_result(&key(), &CallToolResult::success(vec![]));
        assert!(matches!(empty, Err(TicketError::Invalid(_))));
    }

    #[test]
    fn validates_config_and_redacts_env_values() {
        assert!(matches!(JiraMcpSource::new(vec![], "t".into(), vec![]), Err(TicketError::Config(_))));
        assert!(matches!(JiraMcpSource::new(vec![" ".into()], "t".into(), vec![]), Err(TicketError::Config(_))));
        assert!(matches!(JiraMcpSource::new(vec!["uvx".into()], " ".into(), vec![]), Err(TicketError::Config(_))));
        let source = JiraMcpSource::new(
            vec!["uvx".into()],
            "jira_get_issue".into(),
            vec![("JIRA_API_TOKEN".into(), "s3cret".into())],
        )
        .unwrap();
        let debug = format!("{source:?}");
        assert!(debug.contains("JIRA_API_TOKEN") && !debug.contains("s3cret"), "{debug}");
    }

    #[tokio::test]
    async fn missing_program_is_config_error() {
        let source = JiraMcpSource::new(vec!["/nonexistent/qgate-mcp".into()], "t".into(), vec![]).unwrap();
        assert!(matches!(source.fetch(&key()).await, Err(TicketError::Config(_))));
    }

    const LEAK_PROBE: &str = "CARGO_MANIFEST_DIR";

    const FAKE_SERVER: &str = r#"
import json, os, sys
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    if msg["method"] == "initialize":
        result = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fake-jira", "version": "0.0.1"}}
    elif msg["method"] == "tools/call":
        if os.environ.get("FAKE_HANG"):
            continue
        args = msg["params"]["arguments"]
        issue = {"summary": "Fake " + args["issue_key"], "status": {"name": os.environ.get("FAKE_TOKEN", "")},
                 "description": "Acceptance Criteria:\n- leaked=" + str("CARGO_MANIFEST_DIR" in os.environ)}
        result = {"content": [{"type": "text", "text": json.dumps(issue)}], "isError": False}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}) + "\n")
    sys.stdout.flush()
"#;

    fn fake_server(dir: &tempfile::TempDir, env: Vec<(String, String)>) -> Option<JiraMcpSource> {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            eprintln!("python3 not available; skipping MCP process test");
            return None;
        }
        let script = dir.path().join("fake_server.py");
        std::fs::write(&script, FAKE_SERVER).unwrap();
        let command = vec!["python3".into(), "-I".into(), script.display().to_string()];
        Some(JiraMcpSource::new(command, "jira_get_issue".into(), env).unwrap())
    }

    #[tokio::test]
    async fn fetches_from_fake_mcp_server_with_only_configured_env() {
        let dir = tempfile::tempdir().unwrap();
        let Some(source) = fake_server(&dir, vec![("FAKE_TOKEN".into(), "configured".into())]) else {
            return;
        };

        let ticket = source.fetch(&key()).await.unwrap();

        assert_eq!(ticket.title, "Fake ABC-1");
        assert_eq!(ticket.status.as_deref(), Some("configured"));
        assert!(std::env::var_os(LEAK_PROBE).is_some(), "precondition: {LEAK_PROBE} set in the test process");
        assert_eq!(ticket.criteria, vec!["leaked=False"]);
    }

    #[tokio::test]
    async fn times_out_when_server_hangs() {
        let dir = tempfile::tempdir().unwrap();
        let Some(mut source) = fake_server(&dir, vec![("FAKE_HANG".into(), "1".into())]) else {
            return;
        };
        source.timeout = Duration::from_millis(1500);
        let err = source.fetch(&key()).await.unwrap_err();
        assert!(matches!(err, TicketError::Unavailable(ref m) if m.contains("did not answer")), "{err}");
    }

    #[tokio::test]
    #[ignore = "needs uvx, network and real Jira credentials"]
    async fn real_mcp_atlassian() {
        let env = ["JIRA_URL", "JIRA_USERNAME", "JIRA_API_TOKEN"]
            .iter()
            .map(|name| (name.to_string(), std::env::var(name).unwrap()))
            .collect();
        let source =
            JiraMcpSource::new(vec!["uvx".into(), "mcp-atlassian".into()], "jira_get_issue".into(), env).unwrap();
        let key = TicketKey::parse(&std::env::var("QGATE_TEST_ISSUE").unwrap()).unwrap();
        let ticket = source.fetch(&key).await.unwrap();
        println!("{ticket:#?}");
        assert!(!ticket.title.is_empty());
    }
}
