use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::llm::ToolOutput;

struct Recorder {
    calls: Mutex<Vec<(String, Value)>>,
    finish_on: &'static str,
}

impl Recorder {
    fn new(finish_on: &'static str) -> Self {
        Self { calls: Mutex::default(), finish_on }
    }

    fn names(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(n, _)| n.clone()).collect()
    }
}

impl ToolHandler for Recorder {
    async fn call(&self, name: &str, input: Value) -> ToolOutput {
        self.calls.lock().unwrap().push((name.to_string(), input));
        ToolOutput::ok(format!("{name} done"))
    }

    fn finished(&self) -> bool {
        self.calls.lock().unwrap().iter().any(|(n, _)| n == self.finish_on)
    }
}

fn specs() -> Vec<ToolSpec> {
    ["ping", "report_result"]
        .map(|name| ToolSpec {
            name: name.into(),
            description: name.into(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        })
        .to_vec()
}

const LIMITS: LoopLimits = LoopLimits { max_turns: 30 };

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-claude");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn loop_for(bin: &Path, timeout: Duration) -> CliToolLoop {
    let config = CliConfig { claude_bin: bin.into(), timeout, ..CliConfig::default() };
    CliToolLoop::new(config).unwrap().with_finish_grace(Duration::from_millis(300))
}

async fn drive(cli: &CliToolLoop, handler: &Recorder, limits: LoopLimits) -> Result<LoopOutcome, LlmError> {
    let system = [SystemBlock { text: "system rules".into(), cache: true }];
    cli.drive(&system, vec![ContentPart::Text("the task".into())], &specs(), handler, limits).await
}

const MCP_CLIENT: &str = r#"
cfg=""; prev=""
for a in "$@"; do [ "$prev" = "--mcp-config" ] && cfg="$a"; prev="$a"; done
url=$(sed -n 's/.*"url":"\([^"]*\)".*/\1/p' "$cfg")
H='Content-Type: application/json'; A='Accept: application/json, text/event-stream'; V='MCP-Protocol-Version: 2025-06-18'
hdr=$(mktemp)
curl -s -D "$hdr" -H "$H" -H "$A" -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' "$url" >/dev/null
sid=$(grep -i '^mcp-session-id' "$hdr" | awk '{print $2}' | tr -d '\r')
curl -s -H "$H" -H "$A" -H "$V" -H "Mcp-Session-Id: $sid" -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' "$url" >/dev/null
call() {
  curl -s -H "$H" -H "$A" -H "$V" -H "Mcp-Session-Id: $sid" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":{}}}" "$url"
}
"#;

#[tokio::test]
async fn model_tool_calls_reach_the_handler_and_the_report_finishes_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let body = format!(
        "printf '%s\\n' \"$@\" > {args}\ncat > {dir}/prompt.txt\n{MCP_CLIENT}\ncall ping >/dev/null\ncall report_result >/dev/null\nsleep 30",
        args = args_file.display(),
        dir = dir.path().display(),
    );
    let cli = loop_for(&script(dir.path(), &body), Duration::from_secs(60));
    let handler = Recorder::new("report_result");

    let outcome = drive(&cli, &handler, LIMITS).await.unwrap();

    assert_eq!(handler.names(), ["ping", "report_result"]);
    assert_eq!((outcome.stop, outcome.turns), (LoopStop::Finished, 2));
    let args = std::fs::read_to_string(&args_file).unwrap();
    for expected in [
        "--mcp-config",
        "--strict-mcp-config",
        "mcp__qgate__ping,mcp__qgate__report_result",
        "--setting-sources",
        "--no-session-persistence",
        "system rules",
    ] {
        assert!(args.contains(expected), "missing {expected} in:\n{args}");
    }
    assert!(args.contains("--tools\n\n--mcp-config"), "built-in tools must be disabled:\n{args}");
    assert_eq!(std::fs::read_to_string(dir.path().join("prompt.txt")).unwrap(), "the task");
}

#[tokio::test]
async fn a_report_followed_by_a_normal_exit_is_finished() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "cat >/dev/null\n{MCP_CLIENT}\ncall report_result >/dev/null\necho '{{\"is_error\":false,\"result\":\"bye\"}}'"
    );
    let cli = loop_for(&script(dir.path(), &body), Duration::from_secs(60));
    let outcome = drive(&cli, &Recorder::new("report_result"), LIMITS).await.unwrap();
    assert_eq!((outcome.stop, outcome.turns, outcome.final_text.as_str()), (LoopStop::Finished, 1, "bye"));
}

#[tokio::test]
async fn exceeding_the_turn_cap_stops_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!("{MCP_CLIENT}\ncall ping >/dev/null\ncall ping >/dev/null\ncall ping >/dev/null\nsleep 30");
    let cli = loop_for(&script(dir.path(), &body), Duration::from_secs(60));
    let handler = Recorder::new("report_result");

    let outcome = drive(&cli, &handler, LoopLimits { max_turns: 2 }).await.unwrap();

    assert_eq!((outcome.stop, outcome.turns), (LoopStop::MaxTurns, 2));
    assert_eq!(handler.names().len(), 2, "the call over the cap is not executed");
}

#[tokio::test]
async fn a_model_that_stops_without_a_report_yields_no_more_tool_calls_and_its_text() {
    let dir = tempfile::tempdir().unwrap();
    let bin = script(dir.path(), r#"cat >/dev/null; echo '{"type":"result","is_error":false,"result":"all done"}'"#);
    let handler = Recorder::new("report_result");

    let outcome = drive(&loop_for(&bin, Duration::from_secs(60)), &handler, LIMITS).await.unwrap();

    assert_eq!(outcome.stop, LoopStop::NoMoreToolCalls);
    assert_eq!(outcome.final_text, "all done");
}

const BILLED_ENVELOPE: &str = r#"{"type":"result","is_error":false,"result":"done","total_cost_usd":0.42,"usage":{"input_tokens":7,"output_tokens":11,"cache_read_input_tokens":3,"cache_creation_input_tokens":5}}"#;

#[tokio::test]
async fn the_envelope_usage_and_cost_are_reported_on_the_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let bin = script(dir.path(), &format!("cat >/dev/null; echo '{BILLED_ENVELOPE}'"));

    let outcome =
        drive(&loop_for(&bin, Duration::from_secs(60)), &Recorder::new("report_result"), LIMITS).await.unwrap();

    let usage = outcome.usage;
    assert_eq!((usage.input_tokens, usage.output_tokens), (7, 11));
    assert_eq!((usage.cache_read_tokens, usage.cache_write_tokens), (3, 5));
    assert!((outcome.cost_usd - 0.42).abs() < f64::EPSILON);
}

#[tokio::test]
async fn a_finished_session_with_an_envelope_keeps_its_cost() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!("cat >/dev/null\n{MCP_CLIENT}\ncall report_result >/dev/null\necho '{BILLED_ENVELOPE}'");
    let cli = loop_for(&script(dir.path(), &body), Duration::from_secs(60));

    let outcome = drive(&cli, &Recorder::new("report_result"), LIMITS).await.unwrap();

    assert_eq!(outcome.stop, LoopStop::Finished);
    assert_eq!(outcome.usage.output_tokens, 11);
    assert!((outcome.cost_usd - 0.42).abs() < f64::EPSILON);
}

#[tokio::test]
async fn an_error_result_is_classified_and_usage_limits_are_rate_limited() {
    let dir = tempfile::tempdir().unwrap();
    let limited = r#"echo '{"type":"result","is_error":true,"result":"You have hit your limit · resets 4:30pm"}'"#;
    let bin = script(dir.path(), limited);
    let err = drive(&loop_for(&bin, Duration::from_secs(60)), &Recorder::new("x"), LIMITS).await.unwrap_err();
    assert!(matches!(err, LlmError::RateLimited { .. }), "{err:?}");

    let bin = script(dir.path(), r#"echo '{"type":"result","is_error":true,"result":"boom","api_error_status":500}'"#);
    let err = drive(&loop_for(&bin, Duration::from_secs(60)), &Recorder::new("x"), LIMITS).await.unwrap_err();
    assert_eq!(err, LlmError::Unavailable("boom".into()));
}

#[tokio::test]
async fn non_json_output_becomes_an_error_with_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let bin = script(dir.path(), "echo 'not logged in' >&2; exit 1");
    let err = drive(&loop_for(&bin, Duration::from_secs(60)), &Recorder::new("x"), LIMITS).await.unwrap_err();
    assert_eq!(err, LlmError::Unavailable("not logged in".into()));
}

#[tokio::test]
async fn output_over_the_cap_is_an_error_instead_of_unbounded_memory() {
    let dir = tempfile::tempdir().unwrap();
    for stream in ["", ">&2"] {
        let body = format!("cat >/dev/null; head -c 5000000 /dev/zero | tr '\\0' x {stream}");
        let bin = script(dir.path(), &body);

        let err = drive(&loop_for(&bin, Duration::from_secs(60)), &Recorder::new("x"), LIMITS).await.unwrap_err();

        assert!(matches!(&err, LlmError::Unavailable(m) if m.contains("exceeded")), "{stream}: {err:?}");
    }
}

#[tokio::test]
async fn a_session_over_its_deadline_is_killed_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let bin = script(dir.path(), "sleep 30");
    let started = std::time::Instant::now();
    let err = drive(&loop_for(&bin, Duration::from_millis(300)), &Recorder::new("x"), LIMITS).await.unwrap_err();
    assert!(matches!(&err, LlmError::Unavailable(m) if m.contains("did not finish")), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_missing_binary_is_unavailable() {
    let cli = loop_for(Path::new("/nonexistent/claude-bin"), Duration::from_secs(5));
    let err = drive(&cli, &Recorder::new("x"), LIMITS).await.unwrap_err();
    assert!(matches!(&err, LlmError::Unavailable(m) if m.contains("cannot start")), "{err:?}");
}

#[test]
fn invalid_settings_are_rejected() {
    let config = CliConfig { effort: "huge".into(), ..CliConfig::default() };
    assert!(matches!(CliToolLoop::new(config), Err(LlmError::Rejected(_))));
}

#[tokio::test]
#[ignore = "launches the real claude CLI"]
async fn live_claude_calls_a_trivial_mcp_tool() {
    let model = std::env::var("QGATE_SMOKE_MODEL").unwrap_or_else(|_| "haiku".into());
    let config = CliConfig { model, effort: "low".into(), timeout: Duration::from_secs(180), ..CliConfig::default() };
    let cli = CliToolLoop::new(config).unwrap().with_finish_grace(Duration::from_secs(60));
    let handler = Recorder::new("ping");
    let system = [SystemBlock { text: "You only use the tools you are given.".into(), cache: true }];
    let task = "Call the ping tool exactly once, then reply with the text it returned.";

    let outcome = cli.drive(&system, vec![ContentPart::Text(task.into())], &specs(), &handler, LIMITS).await.unwrap();

    println!("outcome: {outcome:?}\ncalls: {:?}", handler.calls.lock().unwrap());
    assert_eq!(handler.names(), ["ping"]);
    assert_eq!(outcome.stop, LoopStop::Finished);
}
