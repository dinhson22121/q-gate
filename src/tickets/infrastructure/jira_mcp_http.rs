use std::sync::Arc;

use chrono::{Duration, Utc};
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::settings::application::CredentialStore;
use crate::tickets::application::{JiraOauthGateway, TicketError, TicketSource, load_bundle, save_bundle};
use crate::tickets::domain::{Ticket, TicketKey, TokenBundle};

use super::jira_mcp::{classify_tool_error, ticket_from_json, ticket_from_text};

const PROTOCOL_VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "Mcp-Session-Id";
const ISSUE_TOOL: &str = "getJiraIssue";
const ISSUE_VIEW: &str = "evidence";
const INITIALIZE_ID: u64 = 1;
const CALL_ID: u64 = 2;
const EXPIRY_SKEW_SECONDS: i64 = 60;
const RECONNECT_HINT: &str = "reconnect Jira in Settings";

pub struct JiraMcpHttpSource {
    http: reqwest::Client,
    resource_url: String,
    store: Arc<dyn CredentialStore>,
    gateway: Arc<dyn JiraOauthGateway>,
}

impl JiraMcpHttpSource {
    pub fn new(
        http: reqwest::Client,
        resource_url: impl Into<String>,
        store: Arc<dyn CredentialStore>,
        gateway: Arc<dyn JiraOauthGateway>,
    ) -> Self {
        Self { http, resource_url: resource_url.into(), store, gateway }
    }

    async fn usable_bundle(&self) -> Result<TokenBundle, TicketError> {
        let bundle = load_bundle(self.store.as_ref())
            .map_err(|e| TicketError::Config(format!("cannot read Jira credentials: {e}")))?
            .ok_or_else(|| TicketError::Config("Jira is not connected; connect it in Settings".into()))?;
        if !bundle.is_expired(Utc::now(), Duration::seconds(EXPIRY_SKEW_SECONDS)) {
            return Ok(bundle);
        }
        if bundle.refresh_token.is_none() {
            return Err(TicketError::Unauthorized(format!("Jira session expired; {RECONNECT_HINT}")));
        }
        let refreshed = self
            .gateway
            .refresh(&bundle)
            .await
            .map_err(|_| TicketError::Unauthorized(format!("Jira token refresh failed; {RECONNECT_HINT}")))?;
        save_bundle(&refreshed, self.store.as_ref())
            .map_err(|e| TicketError::Config(format!("cannot store refreshed Jira credentials: {e}")))?;
        Ok(refreshed)
    }
}

impl TicketSource for JiraMcpHttpSource {
    async fn fetch(&self, key: &TicketKey) -> Result<Ticket, TicketError> {
        let bundle = self.usable_bundle().await?;
        let text = call_get_issue(&self.http, &self.resource_url, &bundle, key).await?;
        Ok(ticket_from_issue_text(key, &text))
    }
}

struct Reply {
    session: Option<String>,
    is_event_stream: bool,
    body: String,
}

async fn post(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    session: Option<&str>,
    body: &Value,
) -> Result<Reply, TicketError> {
    let mut request = http
        .post(url)
        .bearer_auth(access_token)
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .json(body);
    if let Some(session) = session {
        request = request.header(SESSION_HEADER, session);
    }
    let response = request
        .send()
        .await
        .map_err(|e| TicketError::Unavailable(format!("Jira MCP request failed: {}", e.without_url())))?;
    let status = response.status();
    if !status.is_success() {
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            let insufficient = response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("insufficient_scope"));
            let detail = if insufficient {
                "Jira needs additional permissions; open Settings and Connect Jira again to grant them"
            } else {
                RECONNECT_HINT
            };
            return Err(TicketError::Unauthorized(format!("Jira MCP rejected the credentials ({status}); {detail}")));
        }
        return Err(TicketError::Unavailable(format!("Jira MCP answered {status}")));
    }
    let headers = response.headers();
    let session = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string);
    let is_event_stream = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
    let body = response
        .text()
        .await
        .map_err(|e| TicketError::Unavailable(format!("Jira MCP response unreadable: {}", e.without_url())))?;
    Ok(Reply { session, is_event_stream, body })
}

fn event_payloads(body: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            current.push(data.strip_prefix(' ').unwrap_or(data));
        } else if line.trim().is_empty() && !current.is_empty() {
            events.push(current.join("\n"));
            current.clear();
        }
    }
    if !current.is_empty() {
        events.push(current.join("\n"));
    }
    events
}

fn message_with_id(candidates: Vec<Value>, id: u64) -> Option<Value> {
    candidates
        .into_iter()
        .flat_map(|value| match value {
            Value::Array(items) => items,
            other => vec![other],
        })
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(id))
}

fn rpc_message(reply: &Reply, id: u64) -> Result<Value, TicketError> {
    let candidates: Vec<Value> = if reply.is_event_stream {
        event_payloads(&reply.body).iter().filter_map(|data| serde_json::from_str(data).ok()).collect()
    } else {
        serde_json::from_str(reply.body.trim()).into_iter().collect()
    };
    let message = message_with_id(candidates, id)
        .ok_or_else(|| TicketError::Invalid("Jira MCP reply carried no matching JSON-RPC message".into()))?;
    match message.get("error") {
        Some(error) => {
            let text = error.get("message").and_then(Value::as_str).unwrap_or("Jira MCP reported an error");
            Err(classify_rpc_error(text))
        }
        None => Ok(message),
    }
}

fn classify_rpc_error(text: &str) -> TicketError {
    let lower = text.to_lowercase();
    let summary = text.lines().next().unwrap_or("Jira MCP reported an error").to_string();
    if ["unauthorized", "forbidden", "401", "403"].iter().any(|m| lower.contains(m)) {
        TicketError::Unauthorized(format!("{summary}; {RECONNECT_HINT}"))
    } else {
        TicketError::Unavailable(summary)
    }
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0", "id": INITIALIZE_ID, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "qgate", "version": "1"}
        }
    })
}

fn get_issue_request(bundle: &TokenBundle, key: &TicketKey) -> Value {
    json!({
        "jsonrpc": "2.0", "id": CALL_ID, "method": "tools/call",
        "params": {
            "name": ISSUE_TOOL,
            "arguments": {"cloudId": bundle.site_hostname, "issueIdOrKey": key.to_string(), "view": ISSUE_VIEW}
        }
    })
}

pub async fn call_get_issue(
    http: &reqwest::Client,
    url: &str,
    bundle: &TokenBundle,
    key: &TicketKey,
) -> Result<String, TicketError> {
    let token = bundle.access_token.as_str();
    let init = post(http, url, token, None, &initialize_request()).await?;
    rpc_message(&init, INITIALIZE_ID)?;
    let session = init.session.as_deref();
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    post(http, url, token, session, &initialized).await?;
    let reply = post(http, url, token, session, &get_issue_request(bundle, key)).await?;
    tool_text(key, &rpc_message(&reply, CALL_ID)?)
}

fn tool_text(key: &TicketKey, message: &Value) -> Result<String, TicketError> {
    let result = message.get("result").ok_or_else(|| TicketError::Invalid("Jira MCP reply has no result".into()))?;
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
        })
        .unwrap_or_default();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(classify_tool_error(key, &text));
    }
    if text.trim().is_empty() {
        return Err(TicketError::Invalid("MCP tool returned no text content".into()));
    }
    Ok(text)
}

fn ticket_from_issue_text(key: &TicketKey, text: &str) -> Ticket {
    match serde_json::from_str::<Value>(text.trim()) {
        Ok(payload) if payload.is_object() => ticket_from_json(key, payload.get("data").unwrap_or(&payload)),
        _ => ticket_from_text(key, text),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::{DateTime, Utc};
    use futures::future::BoxFuture;
    use wiremock::matchers::{body_partial_json, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::settings::application::CredentialName;
    use crate::settings::application::credentials::fakes::MemoryStore;
    use crate::tickets::application::{Authorization, OauthError, PendingAuthorization};

    const SESSION: &str = "sess-1";

    struct FakeGateway {
        refreshed_token: &'static str,
        fail: bool,
        calls: Mutex<u32>,
    }

    impl FakeGateway {
        fn new(refreshed_token: &'static str, fail: bool) -> Arc<Self> {
            Arc::new(Self { refreshed_token, fail, calls: Mutex::new(0) })
        }

        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    impl JiraOauthGateway for FakeGateway {
        fn begin<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<Authorization, OauthError>> {
            Box::pin(async { Err(OauthError::UnknownState) })
        }

        fn exchange<'a>(
            &'a self,
            _: &'a PendingAuthorization,
            _: &'a str,
        ) -> BoxFuture<'a, Result<TokenBundle, OauthError>> {
            Box::pin(async { Err(OauthError::UnknownState) })
        }

        fn refresh<'a>(&'a self, bundle: &'a TokenBundle) -> BoxFuture<'a, Result<TokenBundle, OauthError>> {
            *self.calls.lock().unwrap() += 1;
            Box::pin(async move {
                if self.fail {
                    return Err(OauthError::Token("denied".into()));
                }
                Ok(TokenBundle {
                    access_token: self.refreshed_token.into(),
                    expires_at: Utc::now() + Duration::hours(1),
                    ..bundle.clone()
                })
            })
        }
    }

    fn bundle(access: &str, expires_at: DateTime<Utc>, refresh: Option<&str>) -> TokenBundle {
        TokenBundle {
            client_id: "cid".into(),
            token_endpoint: "https://auth.example/token".into(),
            access_token: access.into(),
            refresh_token: refresh.map(str::to_string),
            expires_at,
            site_hostname: "acme.atlassian.net".into(),
        }
    }

    fn store_with(bundle: Option<&TokenBundle>) -> Arc<MemoryStore> {
        let store = Arc::new(MemoryStore::default());
        if let Some(bundle) = bundle {
            save_bundle(bundle, store.as_ref()).unwrap();
        }
        store
    }

    fn fresh(access: &str) -> TokenBundle {
        bundle(access, Utc::now() + Duration::hours(1), Some("r"))
    }

    fn key() -> TicketKey {
        TicketKey::parse("KAN-3").unwrap()
    }

    fn issue_payload() -> String {
        json!({"data": {"id": "10017", "key": "KAN-3", "fields": {
            "summary": "Subtask 2.1",
            "description": "Intro\n\n## Acceptance Criteria\n- One\n- Two",
            "status": {"name": "Peer Review", "statusCategory": {"name": "In Progress"}},
            "assignee": null,
            "issuetype": {"id": "10006", "name": "Subtask"},
            "updated": "2026-11-20T10:00:00.000+0000",
            "labels": [], "comment": {"total": 0}
        }}})
        .to_string()
    }

    fn rpc_result(text: &str, is_error: bool) -> Value {
        json!({"jsonrpc": "2.0", "id": CALL_ID, "result": {"content": [{"type": "text", "text": text}], "isError": is_error}})
    }

    fn event_stream(body: String) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "text/event-stream")
    }

    fn sse(message: &Value) -> ResponseTemplate {
        event_stream(format!("event: message\ndata: {message}\n\n"))
    }

    fn json_reply(message: &Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(message)
    }

    async fn mount_handshake(server: &MockServer, token: &str) {
        let bearer = format!("Bearer {token}");
        Mock::given(method("POST"))
            .and(header("authorization", bearer.as_str()))
            .and(body_partial_json(json!({"method": "initialize", "params": {"protocolVersion": PROTOCOL_VERSION}})))
            .respond_with(
                json_reply(&json!({"jsonrpc": "2.0", "id": INITIALIZE_ID, "result": {}}))
                    .insert_header("Mcp-Session-Id", SESSION),
            )
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(header("authorization", bearer.as_str()))
            .and(header("mcp-session-id", SESSION))
            .and(body_partial_json(json!({"method": "notifications/initialized"})))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_call(server: &MockServer, token: &str, response: ResponseTemplate) {
        let bearer = format!("Bearer {token}");
        Mock::given(method("POST"))
            .and(header("authorization", bearer.as_str()))
            .and(header("mcp-session-id", SESSION))
            .and(body_partial_json(json!({"method": "tools/call", "params": {
                "name": "getJiraIssue",
                "arguments": {"cloudId": "acme.atlassian.net", "issueIdOrKey": "KAN-3", "view": "evidence"}
            }})))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    fn source(server: &MockServer, store: Arc<MemoryStore>, gateway: Arc<FakeGateway>) -> JiraMcpHttpSource {
        JiraMcpHttpSource::new(reqwest::Client::new(), server.uri(), store, gateway)
    }

    fn assert_kan3(ticket: &Ticket) {
        assert_eq!(ticket.key, key());
        assert_eq!(ticket.title, "Subtask 2.1");
        assert_eq!(ticket.status.as_deref(), Some("Peer Review"));
        assert_eq!(ticket.issue_type.as_deref(), Some("Subtask"));
        assert_eq!(ticket.updated.as_deref(), Some("2026-11-20T10:00:00.000+0000"));
        assert_eq!(ticket.criteria, vec!["One", "Two"]);
    }

    #[tokio::test]
    async fn fetches_a_ticket_from_a_plain_json_reply() {
        let server = MockServer::start().await;
        mount_handshake(&server, "tok").await;
        mount_call(&server, "tok", json_reply(&rpc_result(&issue_payload(), false))).await;
        let source = source(&server, store_with(Some(&fresh("tok"))), FakeGateway::new("x", false));
        assert_kan3(&source.fetch(&key()).await.unwrap());
    }

    #[tokio::test]
    async fn fetches_a_ticket_from_an_event_stream_reply() {
        let server = MockServer::start().await;
        mount_handshake(&server, "tok").await;
        let noise = json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}});
        let body = format!(
            "event: message\ndata: {noise}\n\nevent: message\ndata: {}\n\n",
            rpc_result(&issue_payload(), false)
        );
        mount_call(&server, "tok", event_stream(body)).await;
        let source = source(&server, store_with(Some(&fresh("tok"))), FakeGateway::new("x", false));
        assert_kan3(&source.fetch(&key()).await.unwrap());
    }

    #[tokio::test]
    async fn empty_description_yields_no_criteria() {
        let server = MockServer::start().await;
        mount_handshake(&server, "tok").await;
        let payload = json!({"data": {"key": "KAN-3", "fields": {"summary": "S", "description": ""}}}).to_string();
        mount_call(&server, "tok", sse(&rpc_result(&payload, false))).await;
        let source = source(&server, store_with(Some(&fresh("tok"))), FakeGateway::new("x", false));
        let ticket = source.fetch(&key()).await.unwrap();
        assert_eq!(ticket.title, "S");
        assert!(ticket.criteria.is_empty());
    }

    #[tokio::test]
    async fn not_connected_is_a_config_error() {
        let server = MockServer::start().await;
        let source = source(&server, store_with(None), FakeGateway::new("x", false));
        let error = source.fetch(&key()).await.unwrap_err();
        assert!(matches!(error, TicketError::Config(ref m) if m.contains("not connected")));
    }

    #[tokio::test]
    async fn expired_bundle_is_refreshed_saved_and_used() {
        let server = MockServer::start().await;
        mount_handshake(&server, "new-tok").await;
        mount_call(&server, "new-tok", json_reply(&rpc_result(&issue_payload(), false))).await;
        let expired = bundle("old-tok", Utc::now() - Duration::minutes(5), Some("r"));
        let store = store_with(Some(&expired));
        let gateway = FakeGateway::new("new-tok", false);
        let source = source(&server, store.clone(), gateway.clone());
        assert_kan3(&source.fetch(&key()).await.unwrap());
        assert_eq!(gateway.calls(), 1);
        assert_eq!(load_bundle(store.as_ref()).unwrap().unwrap().access_token, "new-tok");
        assert!(store.values.lock().unwrap().contains_key(&CredentialName::JiraOauth));
    }

    #[tokio::test]
    async fn expired_bundle_without_refresh_token_asks_to_reconnect() {
        let server = MockServer::start().await;
        let expired = bundle("old", Utc::now() - Duration::minutes(5), None);
        let gateway = FakeGateway::new("x", false);
        let source = source(&server, store_with(Some(&expired)), gateway.clone());
        let error = source.fetch(&key()).await.unwrap_err();
        assert!(matches!(error, TicketError::Unauthorized(ref m) if m.contains("reconnect")));
        assert_eq!(gateway.calls(), 0);
    }

    #[tokio::test]
    async fn refresh_failure_is_unauthorized_without_leaking_details() {
        let server = MockServer::start().await;
        let expired = bundle("old", Utc::now() - Duration::minutes(5), Some("r"));
        let source = source(&server, store_with(Some(&expired)), FakeGateway::new("x", true));
        let error = source.fetch(&key()).await.unwrap_err();
        assert!(matches!(error, TicketError::Unauthorized(ref m) if m.contains("reconnect") && !m.contains("denied")));
    }

    #[tokio::test]
    async fn http_401_is_unauthorized_with_a_reconnect_hint() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
        let source = source(&server, store_with(Some(&fresh("tok"))), FakeGateway::new("x", false));
        let error = source.fetch(&key()).await.unwrap_err();
        assert!(matches!(error, TicketError::Unauthorized(ref m) if m.contains("reconnect Jira in Settings")));
    }

    #[tokio::test]
    async fn is_error_results_are_classified() {
        let cases = [
            ("Forbidden: no permission", "unauthorized"),
            ("Issue does not exist", "not_found"),
            ("boom", "unavailable"),
        ];
        for (text, expected) in cases {
            let server = MockServer::start().await;
            mount_handshake(&server, "tok").await;
            mount_call(&server, "tok", json_reply(&rpc_result(text, true))).await;
            let source = source(&server, store_with(Some(&fresh("tok"))), FakeGateway::new("x", false));
            let kind = match source.fetch(&key()).await.unwrap_err() {
                TicketError::Unauthorized(_) => "unauthorized",
                TicketError::NotFound(_) => "not_found",
                TicketError::Unavailable(_) => "unavailable",
                _ => "other",
            };
            assert_eq!(kind, expected, "{text}");
        }
    }

    #[tokio::test]
    async fn json_rpc_errors_and_unparseable_replies_are_reported() {
        let server = MockServer::start().await;
        mount_handshake(&server, "tok").await;
        let rpc_error = json!({"jsonrpc": "2.0", "id": CALL_ID, "error": {"code": -32000, "message": "bad things"}});
        mount_call(&server, "tok", json_reply(&rpc_error)).await;
        let first = source(&server, store_with(Some(&fresh("tok"))), FakeGateway::new("x", false));
        assert!(matches!(first.fetch(&key()).await, Err(TicketError::Unavailable(_))));

        let server = MockServer::start().await;
        mount_handshake(&server, "tok").await;
        mount_call(&server, "tok", ResponseTemplate::new(200).set_body_string("not json")).await;
        let second = source(&server, store_with(Some(&fresh("tok"))), FakeGateway::new("x", false));
        assert!(matches!(second.fetch(&key()).await, Err(TicketError::Invalid(_))));
    }

    #[test]
    fn event_stream_parsing_joins_multiline_data_and_handles_missing_trailing_blank_line() {
        let body = "event: message\ndata: {\"a\":\ndata: 1}\n\ndata:{\"b\":2}";
        assert_eq!(event_payloads(body), vec!["{\"a\":\n1}".to_string(), "{\"b\":2}".to_string()]);
    }

    #[test]
    fn non_json_issue_text_falls_back_to_markdown() {
        let ticket = ticket_from_issue_text(&key(), "# Title\n\n## Acceptance Criteria\n- A");
        assert_eq!(ticket.title, "Title");
        assert_eq!(ticket.criteria, vec!["A"]);
    }
}
