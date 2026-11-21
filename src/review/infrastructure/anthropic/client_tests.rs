use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::review::domain::change_set::FileChange;
use crate::review::domain::change_set::fixtures::{added, file};
use crate::review::domain::finding::{Category, Severity};
use crate::review::domain::requirements::{AcceptanceCriterion, CriterionStatus, Requirements};

const KEY: &str = "sk-test-key";

fn reviewer(server: &MockServer) -> AnthropicReviewer {
    let settings = AnthropicSettings {
        base_url: format!("{}/", server.uri()),
        retry_base_delay: Duration::from_millis(1),
        ..AnthropicSettings::default()
    };
    AnthropicReviewer::new(KEY.into(), settings).unwrap()
}

fn files() -> Vec<FileChange> {
    vec![file("app.py", 1, vec![added(1, "eval(user_input)")], Some("eval(user_input)\n"))]
}

fn finding_json() -> Value {
    json!({"findings": [{
        "file": "app.py", "line": 1, "severity": "critical", "category": "security", "confidence": "high",
        "title": "eval on user input", "explanation": "RCE", "evidence": "eval(user_input)",
        "suggestion": "use ast.literal_eval",
        "prevention_rule": "Never pass user input to eval.", "rule_scope": "**/*.py"
    }]})
}

fn message(content: Value, stop_reason: &str, stop_details: Value) -> Value {
    json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
        "content": content, "stop_reason": stop_reason, "stop_details": stop_details,
        "usage": {"input_tokens": 1_000_000, "output_tokens": 100_000,
                  "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
    })
}

fn ok_answer(answer: &Value) -> ResponseTemplate {
    let content = json!([
        {"type": "thinking", "thinking": "", "signature": "sig"},
        {"type": "fallback", "from_model": "x"},
        {"type": "text", "text": answer.to_string()}
    ]);
    ResponseTemplate::new(200).set_body_json(message(content, "end_turn", Value::Null))
}

fn error(status: u16, kind: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(json!({"type": "error", "error": {"type": kind, "message": format!("{kind} happened")}}))
}

async fn review(server: &MockServer) -> Result<ReviewerOutput, ReviewerError> {
    let files = files();
    reviewer(server).review(ReviewInput { files: &files, guidelines: "Prefer explicit errors." }).await
}

async fn mount(server: &MockServer, response: ResponseTemplate, times: u64) {
    Mock::given(method("POST")).and(path("/v1/messages")).respond_with(response).expect(times).mount(server).await;
}

#[tokio::test]
async fn parses_findings_usage_and_cost_ignoring_non_text_blocks() {
    let server = MockServer::start().await;
    mount(&server, ok_answer(&finding_json()), 1).await;

    let output = review(&server).await.unwrap();

    assert_eq!(output.findings.len(), 1);
    let finding = &output.findings[0];
    assert_eq!((finding.severity, finding.category), (Severity::Critical, Category::Security));
    assert_eq!(finding.suggestion.as_deref(), Some("use ast.literal_eval"));
    assert_eq!(finding.prevention_rule.as_ref().map(|r| r.text.as_str()), Some("Never pass user input to eval."));
    assert_eq!(output.usage.input_tokens, 1_000_000);
    assert!((output.cost_usd.unwrap() - (4.0 + 2.0)).abs() < 1e-9);
}

#[tokio::test]
async fn sends_expected_request_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2026-10-21"))
        .and(header("anthropic-beta", "server-side-fallback-2026-07-01"))
        .and(header("content-type", "application/json"))
        .respond_with(ok_answer(&json!({"findings": []})))
        .expect(1)
        .mount(&server)
        .await;

    review(&server).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(body["output_config"]["effort"], "high");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["output_config"]["format"]["schema"], review_schema());
    assert_eq!(body["fallbacks"], "default");
    for forbidden in ["temperature", "top_p", "top_k", "tool_choice"] {
        assert!(body.get(forbidden).is_none(), "{forbidden} must not be sent");
    }
    assert_eq!(body["system"][0]["text"], REVIEW_SYSTEM_PROMPT);
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(body["system"][1]["cache_control"], json!({"type": "ephemeral"}));
    assert!(body["system"][1]["text"].as_str().unwrap().contains("Prefer explicit errors."));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert!(messages[0]["content"][0]["text"].as_str().unwrap().contains("1 + eval(user_input)"));
}

#[tokio::test]
async fn refusal_maps_to_refused_with_category() {
    let server = MockServer::start().await;
    let body = message(json!([]), "refusal", json!({"type": "refusal", "category": "cyber", "explanation": null}));
    mount(&server, ResponseTemplate::new(200).set_body_json(body), 1).await;

    assert_eq!(review(&server).await, Err(ReviewerError::Refused { category: Some("cyber".into()) }));
}

#[tokio::test]
async fn max_tokens_maps_to_truncated() {
    let server = MockServer::start().await;
    let body = message(json!([{"type": "text", "text": "{\"findings\": ["}]), "max_tokens", Value::Null);
    mount(&server, ResponseTemplate::new(200).set_body_json(body), 1).await;

    assert_eq!(review(&server).await, Err(ReviewerError::Truncated));
}

#[tokio::test]
async fn invalid_json_answer_maps_to_invalid_response() {
    let server = MockServer::start().await;
    let body = message(json!([{"type": "text", "text": "not json"}]), "end_turn", Value::Null);
    mount(&server, ResponseTemplate::new(200).set_body_json(body), 1).await;

    assert!(matches!(review(&server).await, Err(ReviewerError::InvalidResponse(_))));
}

#[tokio::test]
async fn invalid_envelope_maps_to_invalid_response() {
    let server = MockServer::start().await;
    mount(&server, ResponseTemplate::new(200).set_body_string("<html>"), 1).await;

    assert!(matches!(review(&server).await, Err(ReviewerError::InvalidResponse(_))));
}

#[tokio::test]
async fn rate_limit_is_retried_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(error(429, "rate_limit_error").insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    mount(&server, ok_answer(&finding_json()), 1).await;

    assert_eq!(review(&server).await.unwrap().findings.len(), 1);
}

#[tokio::test]
async fn overloaded_until_retries_exhausted_is_unavailable() {
    let server = MockServer::start().await;
    mount(&server, error(529, "overloaded_error"), u64::from(MAX_RETRIES) + 1).await;

    match review(&server).await {
        Err(ReviewerError::Unavailable(reason)) => assert!(reason.contains("overloaded_error"), "{reason}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn unauthorized_is_rejected_without_retry_and_without_key() {
    let server = MockServer::start().await;
    mount(&server, error(401, "authentication_error"), 1).await;

    match review(&server).await {
        Err(error @ ReviewerError::Rejected(_)) => {
            assert!(error.is_fatal());
            assert!(!error.to_string().contains(KEY));
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn redirect_is_rejected_without_following_or_retrying() {
    let server = MockServer::start().await;
    let target = MockServer::start().await;
    let location = format!("{}/v1/messages", target.uri());
    mount(&server, ResponseTemplate::new(307).insert_header("location", location.as_str()), 1).await;
    mount(&target, ok_answer(&json!({"findings": []})), 0).await;

    match review(&server).await {
        Err(ReviewerError::Rejected(reason)) => {
            assert!(reason.contains("unexpected redirect"), "{reason}");
            assert!(!reason.contains(KEY));
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert!(target.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn should_retry_false_stops_retries() {
    let server = MockServer::start().await;
    mount(&server, error(503, "api_error").insert_header("x-should-retry", "false"), 1).await;

    assert!(matches!(review(&server).await, Err(ReviewerError::Unavailable(_))));
}

#[tokio::test]
async fn should_retry_true_forces_retry_of_client_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(error(400, "invalid_request_error").insert_header("x-should-retry", "true"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    mount(&server, ok_answer(&finding_json()), 1).await;

    assert!(review(&server).await.is_ok());
}

#[tokio::test]
async fn connection_failure_is_unavailable() {
    // Reserve a free port, then release it so nothing listens there.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let settings = AnthropicSettings {
        base_url: format!("http://127.0.0.1:{port}"),
        retry_base_delay: Duration::from_millis(1),
        ..AnthropicSettings::default()
    };
    let files = files();

    let result = AnthropicReviewer::new(KEY.into(), settings)
        .unwrap()
        .review(ReviewInput { files: &files, guidelines: "" })
        .await;

    assert!(matches!(result, Err(ReviewerError::Unavailable(_))), "{result:?}");
}

#[tokio::test]
async fn assess_requirements_sends_own_schema_and_parses_assessments() {
    let server = MockServer::start().await;
    let answer = json!({"assessments": [{
        "criterion_id": "AC-1", "status": "met", "explanation": "implemented",
        "evidence": [{"file": "app.py", "line": 1, "quote": "eval(user_input)"}]
    }]});
    mount(&server, ok_answer(&answer), 1).await;
    let requirements = Requirements {
        key: "QG-7".into(),
        title: "Evaluate input".into(),
        url: None,
        criteria: vec![AcceptanceCriterion { id: "AC-1".into(), text: "Input is evaluated".into() }],
    };
    let files = files();

    let output = reviewer(&server)
        .assess_requirements(RequirementsInput { files: &files, requirements: &requirements })
        .await
        .unwrap();

    assert_eq!(output.assessments[0].status, CriterionStatus::Met);
    assert!(output.cost_usd.is_some());
    let requests: Vec<Request> = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["output_config"]["format"]["schema"], assessment_schema());
    assert_eq!(body["system"][0]["text"], REQUIREMENTS_SYSTEM_PROMPT);
    assert!(body["messages"][0]["content"][0]["text"].as_str().unwrap().contains("<criterion id=\"AC-1\">"));
}

#[test]
fn new_rejects_bad_key_and_settings() {
    let ok = AnthropicSettings::default();
    assert!(matches!(AnthropicReviewer::new("  ".into(), ok.clone()), Err(ReviewerError::Rejected(_))));
    assert!(matches!(AnthropicReviewer::new("bad\nkey".into(), ok.clone()), Err(ReviewerError::Rejected(_))));
    let bad_effort = AnthropicSettings { effort: "extreme".into(), ..ok.clone() };
    assert!(AnthropicReviewer::new(KEY.into(), bad_effort).is_err());
    let bad_url = AnthropicSettings { base_url: "ftp://x".into(), ..ok.clone() };
    assert!(AnthropicReviewer::new(KEY.into(), bad_url).is_err());
    let no_model = AnthropicSettings { model: " ".into(), ..ok.clone() };
    assert!(AnthropicReviewer::new(KEY.into(), no_model).is_err());
    let no_tokens = AnthropicSettings { max_tokens: 0, ..ok };
    assert!(AnthropicReviewer::new(KEY.into(), no_tokens).is_err());
}

#[test]
fn retry_delay_is_exponential_and_honours_capped_retry_after() {
    let base = Duration::from_secs(1);
    assert_eq!(retry_delay(base, 0, None), Duration::from_secs(1));
    assert_eq!(retry_delay(base, 2, None), Duration::from_secs(4));
    assert_eq!(retry_delay(base, 0, Some(Duration::from_secs(7))), Duration::from_secs(7));
    assert_eq!(retry_delay(base, 0, Some(Duration::from_secs(600))), MAX_RETRY_DELAY);
    assert_eq!(parse_retry_after("1.5".into()), Some(Duration::from_millis(1500)));
    assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT".into()), None);
    assert_eq!(parse_retry_after("-1".into()), None);
}

#[test]
fn status_classification() {
    let retry = |s: u16, h: Option<bool>| {
        matches!(classify_status(StatusCode::from_u16(s).unwrap(), h, None, String::new()), Failure::Retry { .. })
    };
    for status in [408, 409, 429, 500, 529] {
        assert!(retry(status, None), "{status} should retry");
    }
    for status in [400, 401, 403, 404, 413] {
        assert!(!retry(status, None), "{status} should not retry");
        assert!(matches!(
            classify_status(StatusCode::from_u16(status).unwrap(), None, None, String::new()),
            Failure::Fatal(ReviewerError::Rejected(_))
        ));
    }
    assert!(!retry(429, Some(false)));
    assert!(retry(404, Some(true)));
}
