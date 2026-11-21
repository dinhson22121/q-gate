use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Barrier;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;

const KEY: &str = "sk-test-key";

fn client_with(server: &MockServer, budget: Budget) -> ClaudeClient {
    let config = ClaudeConfig {
        base_url: format!("{}/", server.uri()),
        retry_base_delay: Duration::from_millis(1),
        ..ClaudeConfig::default()
    };
    ClaudeClient::new(KEY.into(), config, budget).unwrap()
}

fn client(server: &MockServer) -> ClaudeClient {
    client_with(server, Budget::unlimited())
}

fn message(content: &Value, stop_reason: &str) -> Value {
    json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
        "content": content, "stop_reason": stop_reason, "stop_details": null,
        "usage": {"input_tokens": 1_000_000, "output_tokens": 100_000,
                  "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
    })
}

fn reply(content: &Value, stop_reason: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(message(content, stop_reason))
}

fn text_reply(text: &str, stop_reason: &str) -> ResponseTemplate {
    reply(&json!([{"type": "text", "text": text}]), stop_reason)
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": input})
}

fn error(status: u16, kind: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(json!({"type": "error", "error": {"type": kind, "message": format!("{kind} happened")}}))
}

/// Serves the given responses in order, one per request.
struct Sequence {
    responses: Vec<ResponseTemplate>,
    next: AtomicUsize,
}

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        self.responses.get(index).cloned().unwrap_or_else(|| ResponseTemplate::new(500))
    }
}

async fn serve(server: &MockServer, responses: Vec<ResponseTemplate>) {
    let expected = responses.len() as u64;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Sequence { responses, next: AtomicUsize::new(0) })
        .expect(expected)
        .mount(server)
        .await;
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    server.received_requests().await.unwrap().iter().map(|r| serde_json::from_slice(&r.body).unwrap()).collect()
}

fn system() -> Vec<SystemBlock> {
    vec![SystemBlock { text: "rules".into(), cache: false }, SystemBlock { text: "big context".into(), cache: true }]
}

fn user() -> Vec<ContentPart> {
    vec![ContentPart::Text("go".into())]
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Verdict {
    ok: bool,
}

async fn structured(client: &ClaudeClient) -> Result<Verdict, LlmError> {
    client.structured::<Verdict>(&system(), user(), json!({"type": "object"})).await
}

// ---------- structured ----------

#[tokio::test]
async fn structured_sends_current_api_shape_and_parses_answer() {
    let server = MockServer::start().await;
    let content = json!([
        {"type": "thinking", "thinking": "", "signature": "sig"},
        {"type": "fallback", "from_model": "x"},
        {"type": "text", "text": "{\"ok\": true}"}
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2026-10-21"))
        .and(header("anthropic-beta", "server-side-fallback-2026-07-01"))
        .and(header("content-type", "application/json"))
        .respond_with(reply(&content, "end_turn"))
        .expect(1)
        .mount(&server)
        .await;
    let budget = Budget::new(100.0);

    let verdict = structured(&client_with(&server, budget.clone())).await.unwrap();

    assert_eq!(verdict, Verdict { ok: true });
    let body = &bodies(&server).await[0];
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(
        body["output_config"],
        json!({"effort": "high", "format": {"type": "json_schema", "schema": {"type": "object"}}})
    );
    assert_eq!(body["fallbacks"], "default");
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(body["system"][1]["cache_control"], json!({"type": "ephemeral"}));
    for forbidden in ["temperature", "top_p", "top_k", "tools", "tool_choice"] {
        assert!(body.get(forbidden).is_none(), "{forbidden} must not be sent");
    }
    assert!(body["thinking"].get("budget_tokens").is_none());
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    // 1M input * $4 + 100k output * $20 per MTok.
    assert!((budget.spent_usd() - 6.0).abs() < 1e-9);
    assert_eq!(budget.usage().output_tokens, 100_000);
}

#[tokio::test]
async fn structured_without_fallbacks_omits_beta_and_field() {
    let server = MockServer::start().await;
    serve(&server, vec![text_reply("{\"ok\": false}", "end_turn")]).await;
    let config = ClaudeConfig {
        base_url: server.uri(),
        use_fallbacks: false,
        retry_base_delay: Duration::from_millis(1),
        ..ClaudeConfig::default()
    };
    let client = ClaudeClient::new(KEY.into(), config, Budget::unlimited()).unwrap();

    assert_eq!(structured(&client).await.unwrap(), Verdict { ok: false });

    let request = &server.received_requests().await.unwrap()[0];
    assert!(request.headers.get("anthropic-beta").is_none());
    assert!(bodies(&server).await[0].get("fallbacks").is_none());
}

#[tokio::test]
async fn refusal_maps_to_refused_with_category() {
    let server = MockServer::start().await;
    let mut body = message(&json!([]), "refusal");
    body["stop_details"] = json!({"type": "refusal", "category": "cyber"});
    serve(&server, vec![ResponseTemplate::new(200).set_body_json(body)]).await;
    let budget = Budget::new(100.0);

    let error = structured(&client_with(&server, budget.clone())).await.unwrap_err();

    assert_eq!(error, LlmError::Refused { category: Some("cyber".into()) });
    assert!(budget.spent_usd() > 0.0, "refused responses are still billed");
}

#[tokio::test]
async fn max_tokens_maps_to_truncated() {
    let server = MockServer::start().await;
    serve(&server, vec![text_reply("{\"ok\":", "max_tokens")]).await;
    assert_eq!(structured(&client(&server)).await.unwrap_err(), LlmError::Truncated);
}

#[tokio::test]
async fn schema_mismatch_and_empty_text_are_invalid_responses() {
    let server = MockServer::start().await;
    serve(&server, vec![text_reply("{\"ok\": true, \"extra\": 1}", "end_turn"), reply(&json!([]), "end_turn")]).await;
    let client = client(&server);

    assert!(matches!(structured(&client).await, Err(LlmError::InvalidResponse(m)) if m.contains("schema")));
    assert!(matches!(structured(&client).await, Err(LlmError::InvalidResponse(m)) if m.contains("no text")));
}

#[tokio::test]
async fn structured_resends_after_pause_turn_and_joins_text() {
    let server = MockServer::start().await;
    serve(&server, vec![text_reply("{\"ok\"", "pause_turn"), text_reply(": true}", "end_turn")]).await;

    assert_eq!(structured(&client(&server)).await.unwrap(), Verdict { ok: true });

    let second = &bodies(&server).await[1];
    assert_eq!(second["messages"][1]["role"], "assistant");
    assert_eq!(second["messages"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn structured_gives_up_when_always_paused() {
    let server = MockServer::start().await;
    serve(&server, (0..=MAX_STRUCTURED_RESUMES).map(|_| text_reply("", "pause_turn")).collect()).await;
    assert!(matches!(structured(&client(&server)).await, Err(LlmError::InvalidResponse(m)) if m.contains("paused")));
}

// ---------- transport ----------

#[tokio::test]
async fn retries_429_then_succeeds() {
    let server = MockServer::start().await;
    serve(
        &server,
        vec![
            error(429, "rate_limit_error").insert_header("retry-after", "0"),
            text_reply("{\"ok\": true}", "end_turn"),
        ],
    )
    .await;
    assert_eq!(structured(&client(&server)).await.unwrap(), Verdict { ok: true });
}

#[tokio::test]
async fn persistent_overload_is_unavailable_after_three_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(error(529, "overloaded_error")).expect(4).mount(&server).await;
    let error = structured(&client(&server)).await.unwrap_err();
    assert_eq!(error, LlmError::Unavailable("HTTP 529 overloaded_error: overloaded_error happened".into()));
}

#[tokio::test]
async fn unauthorized_is_rejected_without_retry_or_key_leak() {
    let server = MockServer::start().await;
    serve(&server, vec![error(401, "authentication_error")]).await;

    let error = structured(&client(&server)).await.unwrap_err();

    assert!(matches!(&error, LlmError::Rejected(m) if m.contains("authentication_error")));
    assert!(!error.to_string().contains(KEY));
}

#[tokio::test]
async fn redirect_is_rejected_without_following() {
    let server = MockServer::start().await;
    serve(&server, vec![ResponseTemplate::new(307).insert_header("location", "https://evil.example/v1/messages")])
        .await;
    assert!(matches!(structured(&client(&server)).await, Err(LlmError::Rejected(m)) if m.contains("redirect")));
}

#[tokio::test]
async fn garbage_success_body_is_invalid_response() {
    let server = MockServer::start().await;
    serve(&server, vec![ResponseTemplate::new(200).set_body_string("not json")]).await;
    assert!(matches!(structured(&client(&server)).await, Err(LlmError::InvalidResponse(m)) if m.contains("envelope")));
}

#[tokio::test]
async fn connection_failure_is_unavailable() {
    // Port 1 has no listener (a dropped MockServer goes back to wiremock's pool and keeps serving).
    let config = ClaudeConfig {
        base_url: "http://127.0.0.1:1".into(),
        retry_base_delay: Duration::from_millis(1),
        ..ClaudeConfig::default()
    };
    let client = ClaudeClient::new(KEY.into(), config, Budget::unlimited()).unwrap();
    assert!(matches!(structured(&client).await, Err(LlmError::Unavailable(_))));
}

#[test]
fn invalid_config_is_rejected() {
    let config = ClaudeConfig { effort: "extreme".into(), ..ClaudeConfig::default() };
    assert!(matches!(ClaudeClient::new(KEY.into(), config, Budget::unlimited()), Err(LlmError::Rejected(_))));
}

#[tokio::test]
async fn exhausted_budget_blocks_before_any_request() {
    let server = MockServer::start().await;
    serve(&server, vec![]).await;
    let budget = Budget::new(5.0);
    budget.record("claude-opus-5-5", &Usage { input_tokens: 2_000_000, ..Usage::default() });

    let error = structured(&client_with(&server, budget)).await.unwrap_err();

    assert_eq!(error, LlmError::BudgetExceeded { limit_usd: 5.0 });
}

// ---------- tool loop ----------

/// Records calls; `slow` tool waits on a barrier so the test proves calls of one turn run concurrently.
struct Recorder {
    calls: Mutex<Vec<(String, Value)>>,
    barrier: Barrier,
    finish_on: Option<&'static str>,
    done: AtomicBool,
}

impl Recorder {
    fn new(finish_on: Option<&'static str>) -> Self {
        Self { calls: Mutex::default(), barrier: Barrier::new(2), finish_on, done: AtomicBool::new(false) }
    }
}

impl ToolHandler for Recorder {
    async fn call(&self, name: &str, input: Value) -> ToolOutput {
        self.calls.lock().unwrap().push((name.to_string(), input.clone()));
        if self.finish_on == Some(name) {
            self.done.store(true, Ordering::SeqCst);
        }
        match name {
            "read" => {
                tokio::time::timeout(Duration::from_secs(5), self.barrier.wait()).await.expect("calls not concurrent");
                ToolOutput::ok(format!("read {}", input["path"].as_str().unwrap_or_default()))
            }
            "fail" => ToolOutput::error("boom"),
            _ => ToolOutput::ok("ok"),
        }
    }

    fn finished(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

fn specs() -> Vec<ToolSpec> {
    ["read", "fail", "report"]
        .iter()
        .map(|name| ToolSpec {
            name: (*name).into(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        })
        .collect()
}

async fn run(server: &MockServer, handler: &Recorder, max_turns: u32) -> Result<LoopOutcome, LlmError> {
    client(server).run_tools(&system(), user(), &specs(), handler, LoopLimits { max_turns }).await
}

#[tokio::test]
async fn parallel_tool_calls_return_all_results_in_one_user_message_in_order() {
    let server = MockServer::start().await;
    let turn = json!([
        {"type": "text", "text": "reading both"},
        tool_use("toolu_a", "read", json!({"path": "a.rs"})),
        tool_use("toolu_b", "read", json!({"path": "b.rs"}))
    ]);
    serve(&server, vec![reply(&turn, "tool_use"), text_reply("all good", "end_turn")]).await;
    let handler = Recorder::new(None);

    let outcome = run(&server, &handler, 10).await.unwrap();

    assert_eq!(outcome, LoopOutcome { turns: 2, stop: LoopStop::NoMoreToolCalls, final_text: "all good".into() });
    let requests = bodies(&server).await;
    let first = &requests[0];
    assert_eq!(first["output_config"], json!({"effort": "high"}));
    assert!(first.get("tool_choice").is_none());
    let tools = first["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(tools[2]["cache_control"], json!({"type": "ephemeral"}));
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0], first["messages"][0], "earlier messages are never edited");
    assert_eq!(messages[1], json!({"role": "assistant", "content": turn}));
    assert_eq!(
        messages[2],
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_a", "content": "read a.rs", "is_error": false},
            {"type": "tool_result", "tool_use_id": "toolu_b", "content": "read b.rs", "is_error": false}
        ]})
    );
}

#[tokio::test]
async fn assistant_content_with_thinking_is_echoed_byte_identical() {
    let server = MockServer::start().await;
    // Deliberately non-alphabetical key order and odd spacing: a re-serialised Value would differ.
    let raw_content = r#"[{"type":"thinking","thinking":"plan: call fail","signature":"EqQBCkYI+/sig=="} ,{"type":"fallback","from_model":"claude-opus-5-5"},{"type":"tool_use","name":"fail","id":"toolu_1","input":{"z":1,"a":2}}]"#;
    let first = format!(
        r#"{{"model":"claude-opus-5-5","content":{raw_content},"stop_reason":"tool_use","stop_details":null,"usage":{{"input_tokens":10,"output_tokens":5}}}}"#
    );
    serve(
        &server,
        vec![ResponseTemplate::new(200).set_body_raw(first, "application/json"), text_reply("done", "end_turn")],
    )
    .await;
    let handler = Recorder::new(None);

    run(&server, &handler, 5).await.unwrap();

    let second = String::from_utf8(server.received_requests().await.unwrap()[1].body.clone()).unwrap();
    assert!(second.contains(&format!(r#"{{"role":"assistant","content":{raw_content}}}"#)), "{second}");
    assert_eq!(handler.calls.lock().unwrap()[0], ("fail".to_string(), json!({"z": 1, "a": 2})));
}

#[tokio::test]
async fn tool_errors_unknown_tools_and_non_object_input_are_reported_as_errors() {
    let server = MockServer::start().await;
    let turn = json!([
        tool_use("t1", "fail", json!({})),
        tool_use("t2", "delete_repo", json!({})),
        tool_use("t3", "report", json!("not an object"))
    ]);
    serve(&server, vec![reply(&turn, "tool_use"), text_reply("sorry", "end_turn")]).await;
    let handler = Recorder::new(None);

    run(&server, &handler, 5).await.unwrap();

    let results = bodies(&server).await[1]["messages"][2]["content"].clone();
    assert_eq!(results[0], json!({"type": "tool_result", "tool_use_id": "t1", "content": "boom", "is_error": true}));
    assert_eq!(results[1]["is_error"], true);
    assert!(results[1]["content"].as_str().unwrap().contains("unknown tool"));
    assert_eq!(results[2]["is_error"], true);
    assert!(results[2]["content"].as_str().unwrap().contains("JSON object"));
    assert_eq!(handler.calls.lock().unwrap().len(), 1, "invalid calls never reach the handler");
}

#[tokio::test]
async fn stops_at_max_turns() {
    let server = MockServer::start().await;
    let turn = json!([tool_use("t", "report", json!({}))]);
    serve(&server, vec![reply(&turn, "tool_use"), reply(&turn, "tool_use")]).await;

    let outcome = run(&server, &Recorder::new(None), 2).await.unwrap();

    assert_eq!((outcome.turns, outcome.stop), (2, LoopStop::MaxTurns));
}

#[tokio::test]
async fn handler_finished_stops_the_loop_without_another_request() {
    let server = MockServer::start().await;
    let turn = json!([{"type": "text", "text": "reporting"}, tool_use("t", "report", json!({"path": "x"}))]);
    serve(&server, vec![reply(&turn, "tool_use")]).await;

    let outcome = run(&server, &Recorder::new(Some("report")), 10).await.unwrap();

    assert_eq!(outcome, LoopOutcome { turns: 1, stop: LoopStop::Finished, final_text: "reporting".into() });
}

#[tokio::test]
async fn pause_turn_is_resent_without_a_new_user_message() {
    let server = MockServer::start().await;
    serve(&server, vec![text_reply("thinking...", "pause_turn"), text_reply("final", "end_turn")]).await;

    let outcome = run(&server, &Recorder::new(None), 5).await.unwrap();

    assert_eq!(outcome, LoopOutcome { turns: 2, stop: LoopStop::NoMoreToolCalls, final_text: "final".into() });
    let messages = bodies(&server).await[1]["messages"].as_array().unwrap().clone();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["role"], "assistant");
}

#[tokio::test]
async fn tool_use_stop_without_tool_blocks_is_invalid() {
    let server = MockServer::start().await;
    serve(&server, vec![text_reply("hmm", "tool_use")]).await;
    assert!(matches!(run(&server, &Recorder::new(None), 5).await, Err(LlmError::InvalidResponse(_))));
}

#[tokio::test]
async fn budget_exhausted_mid_loop_stops_with_budget_exceeded() {
    let server = MockServer::start().await;
    let turn = json!([tool_use("t", "report", json!({}))]);
    serve(&server, vec![reply(&turn, "tool_use")]).await;
    // One response costs $6; the second request must not be sent.
    let client = client_with(&server, Budget::new(5.0));

    let result = client.run_tools(&system(), user(), &specs(), &Recorder::new(None), LoopLimits { max_turns: 5 }).await;

    assert_eq!(result.unwrap_err(), LlmError::BudgetExceeded { limit_usd: 5.0 });
}
