use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::conventions::domain::evidence::ChangedFile;
use crate::conventions::domain::rulebook::RuleStatus;
use crate::llm::{Budget, ClaudeConfig};

fn reply(answer: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
        "content": [{"type": "text", "text": answer.to_string()}], "stop_reason": "end_turn", "stop_details": null,
        "usage": {"input_tokens": 10, "output_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
    }))
}

async fn model(server: &MockServer, answer: Value) -> ClaudeRuleModel {
    Mock::given(method("POST")).and(path("/v1/messages")).respond_with(reply(&answer)).expect(1).mount(server).await;
    let config =
        ClaudeConfig { base_url: server.uri(), retry_base_delay: Duration::from_millis(1), ..ClaudeConfig::default() };
    ClaudeRuleModel::new(Arc::new(ClaudeClient::new("sk-test".into(), config, Budget::unlimited()).unwrap()))
}

async fn body(server: &MockServer) -> Value {
    serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap()
}

fn rule() -> Rule {
    Rule {
        id: "R1".into(),
        title: "Domain is pure".into(),
        description: "No IO.".into(),
        kind: RuleKind::Explicit,
        status: RuleStatus::Approved,
        scope_globs: Vec::new(),
        examples: vec![FileLine { file: "CLAUDE.md".into(), line: 2, quote: "domain is pure".into() }],
        lint_suggestion: None,
    }
}

#[tokio::test]
async fn draft_call_sends_numbered_sources_and_parses_rules() {
    let server = MockServer::start().await;
    let answer = json!({"rules": [{
        "title": "Domain is pure", "description": "d", "kind": "explicit", "scope_globs": ["src/**"],
        "examples": [{"file": "CLAUDE.md", "line": 2, "quote": "domain is pure"}], "lint_suggestion": ""
    }]});
    let model = model(&server, answer).await;
    let request = DraftRequest {
        sources: vec![SourceFile { path: "CLAUDE.md".into(), content: "# R\ndomain is pure\n".into() }],
        samples: vec![SourceFile { path: "src/a\"x.rs".into(), content: "use a;\n".into() }],
        layout: vec!["src".into()],
        languages: vec![("rust".into(), 3)],
    };
    let drafts = model.draft_rules(&request).await.unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].kind, RuleKind::Explicit);
    assert_eq!(drafts[0].lint_suggestion, None);
    let body = body(&server).await;
    assert_eq!(body["system"][0]["text"], BUILD_SYSTEM_PROMPT);
    assert_eq!(body["output_config"]["format"]["schema"], rules_schema());
    let user = body["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert!(user.contains("<rule_source path=\"CLAUDE.md\">\n    1| # R\n    2| domain is pure\n</rule_source>"));
    assert!(user.contains("<code_sample path=\"src/a&quot;x.rs\">"));
    assert!(user.contains("rust: 3 files") && user.contains("src/\n"));
}

#[tokio::test]
async fn check_call_caches_the_rulebook_block_and_parses_violations() {
    let server = MockServer::start().await;
    let answer = json!({"violations": [{
        "rule_id": "R1", "file": "a.rs", "line": 2, "evidence": "std::fs::read()",
        "explanation": "IO in domain", "suggestion": "inject a port"
    }]});
    let model = model(&server, answer).await;
    let rules = [rule()];
    let files = [ChangedFile { path: "a.rs".into(), diff: "    2 + std::fs::read()\n".into(), new_lines: Vec::new() }];
    let request = CheckRequest { rules: &rules, selected: &["R1"], files: &files, context: Some("ticket") };
    let found = model.find_violations(request).await.unwrap();
    assert_eq!(found[0].rule_id, "R1");
    assert_eq!(found[0].suggestion.as_deref(), Some("inject a port"));
    let body = body(&server).await;
    assert_eq!(body["system"][1]["text"], rulebook_text(&rules));
    assert_eq!(body["system"][1]["cache_control"], json!({"type": "ephemeral"}));
    assert!(body["system"][0].get("cache_control").is_none());
    let user = body["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert!(user.starts_with("<rules_to_check>R1</rules_to_check>\n<ticket_context>\nticket\n</ticket_context>"));
    assert!(user.contains("<changed_file path=\"a.rs\">\n    2 + std::fs::read()\n</changed_file>"));
}

#[tokio::test]
async fn model_errors_are_mapped() {
    let server = MockServer::start().await;
    let model = model(&server, json!({"unexpected": true})).await;
    let rules = [rule()];
    let request = CheckRequest { rules: &rules, selected: &["R1"], files: &[], context: None };
    assert!(matches!(model.find_violations(request).await, Err(ConventionsError::Model(_))));
}

#[test]
fn rulebook_text_is_deterministic_and_escaped() {
    let mut scoped = rule();
    scoped.scope_globs = vec!["src/**".into(), "lib/\"x\"".into()];
    let text = rulebook_text(&[rule(), scoped]);
    assert_eq!(
        text,
        rulebook_text(&[rule(), {
            let mut again = rule();
            again.scope_globs = vec!["src/**".into(), "lib/\"x\"".into()];
            again
        }])
    );
    assert!(text.contains("scope=\"**\""));
    assert!(text.contains("scope=\"src/**, lib/&quot;x&quot;\""));
    assert!(text.contains("example CLAUDE.md:2: domain is pure"));
}
