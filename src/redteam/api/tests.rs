use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, ORIGIN};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::history::api::{DashboardState, serve_on_with};
use crate::history::infrastructure::SqliteRunRepository;
use crate::llm::FakeModel;
use crate::redteam::application::{RedteamService, RunPentest};
use crate::redteam::domain::ScanBudget;
use crate::redteam::infrastructure::{AllowlistTargetPolicy, ClaudeAttackModel, ReqwestProbe, SqliteScanStore};
use crate::web::{REQUEST_HEADER, TOKEN_HEADER};

struct TestServer {
    addr: SocketAddr,
    client: reqwest::Client,
    token: ApiToken,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start(model_answer: Value, allowed_targets: &[String]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runs = Arc::new(SqliteRunRepository::open(dir.path().join("qgate.db")).unwrap());
        let hosts = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let token = ApiToken::generate().unwrap();

        let policy = Arc::new(AllowlistTargetPolicy::new(allowed_targets, &[]));
        let model = Arc::new(ClaudeAttackModel::new(Arc::new(FakeModel::answering(model_answer))));
        let probe = Arc::new(ReqwestProbe::new(policy.clone(), Duration::from_secs(5)).unwrap());
        let run_pentest = RunPentest::new(model, probe, policy, ScanBudget::default());
        let store = Arc::new(SqliteScanStore::open(dir.path().join("redteam.db")).unwrap());
        let service = Arc::new(RedteamService::new(run_pentest, store));

        let extra = redteam_router(RedteamApiState::new(service, hosts.clone(), token.clone()));
        let dashboard = DashboardState::with_allowed_hosts(runs, hosts);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_on_with(listener, dashboard, extra, async {
            let _ = stopped.await;
        }));
        Self { addr, client: reqwest::Client::new(), token, _stop: stop, _dir: dir }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn page_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, self.url(path))
            .header(CONTENT_TYPE, "application/json")
            .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
            .header(ORIGIN, format!("http://localhost:{}", self.addr.port()))
    }

    fn mutate(&self, path: &str, body: Value) -> reqwest::RequestBuilder {
        self.page_request(reqwest::Method::POST, path).header(TOKEN_HEADER, self.token.expose()).body(body.to_string())
    }

    async fn send(request: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let response = request.send().await.unwrap();
        (response.status(), response.json().await.unwrap_or(Value::Null))
    }
}

async fn vulnerable_target() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "QGATE-CANARY-1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("results for QGATE-CANARY-1"))
        .mount(&server)
        .await;
    server
}

fn sqli_hypothesis() -> Value {
    json!({
        "hypotheses": [{
            "category": "sql_injection",
            "rationale": "reflected parameter",
            "request": { "method": "GET", "path": "/search?q=QGATE-CANARY-1", "headers": [], "body": null },
            "expected_evidence": { "kind": "body_contains", "value": "QGATE-CANARY-1" },
            "impact": "leaks data",
            "fix": "use parameterized queries",
            "destructive": false,
        }]
    })
}

#[tokio::test]
async fn page_is_self_contained_and_carries_the_token() {
    let target = vulnerable_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(sqli_hypothesis(), &[host]).await;
    let response = server.client.get(server.url("/redteam")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = response.text().await.unwrap();
    assert!(!html.contains("__QGATE_TOKEN__"));
    assert!(html.contains(server.token.expose()));
}

#[tokio::test]
async fn a_scan_proves_the_seeded_vulnerability_and_is_listed_and_fetchable() {
    let target = vulnerable_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(sqli_hypothesis(), &[host]).await;

    let (status, body) =
        TestServer::send(server.mutate("/api/redteam/scan", json!({ "base_url": target.uri() }))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["status"], "completed");
    assert_eq!(body["findings"].as_array().unwrap().len(), 1);
    assert_eq!(body["findings"][0]["category"], "sql_injection");
    assert!(body["findings"][0]["poc"]["response_evidence"].as_str().unwrap().contains("QGATE-CANARY-1"));
    let id = body["id"].as_i64().unwrap();

    let (status, list) = TestServer::send(server.client.get(server.url("/api/redteam/scans"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["scans"].as_array().unwrap().len(), 1);

    let (status, fetched) = TestServer::send(server.client.get(server.url(&format!("/api/redteam/scans/{id}")))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["id"], id);
}

#[tokio::test]
async fn a_non_working_hypothesis_is_rejected_as_unproven() {
    let target = vulnerable_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let answer = json!({
        "hypotheses": [{
            "category": "sql_injection",
            "rationale": "r",
            "request": { "method": "GET", "path": "/search?q=NOT-THE-CANARY", "headers": [], "body": null },
            "expected_evidence": { "kind": "body_contains", "value": "QGATE-CANARY-DOES-NOT-APPEAR" },
            "impact": "i",
            "fix": "f",
            "destructive": false,
        }]
    });
    let server = TestServer::start(answer, &[host]).await;
    let (status, body) =
        TestServer::send(server.mutate("/api/redteam/scan", json!({ "base_url": target.uri() }))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["findings"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_target_outside_the_allowlist_is_refused() {
    let target = vulnerable_target().await;
    let server = TestServer::start(sqli_hypothesis(), &["definitely-not-this-host.example".to_string()]).await;
    let (status, body) =
        TestServer::send(server.mutate("/api/redteam/scan", json!({ "base_url": target.uri() }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("allowlist"));
}

#[tokio::test]
async fn mutations_without_the_right_headers_are_rejected() {
    let target = vulnerable_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(sqli_hypothesis(), &[host]).await;
    let response = server
        .client
        .post(server.url("/api/redteam/scan"))
        .header(CONTENT_TYPE, "application/json")
        .body(json!({ "base_url": target.uri() }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

async fn ssti_target() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/render"))
        .and(query_param("tpl", "{{7*7}}"))
        .respond_with(ResponseTemplate::new(200).set_body_string("result: 49"))
        .mount(&server)
        .await;
    server
}

fn ssti_hypothesis(expected: &str) -> Value {
    json!({
        "hypotheses": [{
            "category": "ssti",
            "rationale": "template parameter reflected unescaped",
            "request": { "method": "GET", "path": "/render?tpl=%7B%7B7*7%7D%7D", "headers": [], "body": null },
            "expected_evidence": { "kind": "body_contains", "value": expected },
            "impact": "arbitrary template evaluation can escalate to remote code execution",
            "fix": "render user input as data, never as a template expression",
            "destructive": false,
        }]
    })
}

#[tokio::test]
async fn an_ssti_hypothesis_is_confirmed_when_the_arithmetic_canary_comes_back() {
    let target = ssti_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(ssti_hypothesis("49"), &[host]).await;
    let (status, body) =
        TestServer::send(server.mutate("/api/redteam/scan", json!({ "base_url": target.uri() }))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["findings"].as_array().unwrap().len(), 1);
    assert_eq!(body["findings"][0]["category"], "ssti");
    assert_eq!(
        body["findings"][0]["rule"]["text"],
        "Prevent server-side template injection: render user input as data, never as a template expression"
    );
}

#[tokio::test]
async fn an_ssti_hypothesis_without_the_canary_in_the_response_is_rejected() {
    let target = ssti_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(ssti_hypothesis("QGATE-WRONG-CANARY"), &[host]).await;
    let (status, body) =
        TestServer::send(server.mutate("/api/redteam/scan", json!({ "base_url": target.uri() }))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["findings"].as_array().unwrap().len(), 0);
}

async fn login_and_idor_target() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("{}").insert_header("set-cookie", "sid=s3cr3t; HttpOnly"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/2"))
        .and(wiremock::matchers::header("cookie", "sid=s3cr3t"))
        .respond_with(ResponseTemplate::new(200).set_body_string("QGATE-OTHER-USER-EMAIL=bob@example.com"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/2"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;
    server
}

fn idor_hypothesis() -> Value {
    json!({
        "hypotheses": [{
            "category": "auth_bypass",
            "rationale": "object id in the path is attacker-controlled while authenticated as a different user",
            "request": { "method": "GET", "path": "/users/2", "headers": [], "body": null },
            "expected_evidence": { "kind": "body_contains", "value": "QGATE-OTHER-USER-EMAIL" },
            "impact": "any authenticated user can read any other user's profile",
            "fix": "check that the authenticated user owns the requested resource before returning it",
            "destructive": false,
        }]
    })
}

#[tokio::test]
async fn a_login_authenticated_scan_carries_the_session_into_an_idor_attack() {
    let target = login_and_idor_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(idor_hypothesis(), &[host]).await;
    let scan_target = json!({
        "base_url": target.uri(),
        "auth": { "type": "login", "url": format!("{}/login", target.uri()), "username": "alice", "password": "pw" },
    });
    let (status, body) = TestServer::send(server.mutate("/api/redteam/scan", scan_target)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["findings"].as_array().unwrap().len(), 1);
    assert_eq!(body["findings"][0]["category"], "auth_bypass");
    assert!(body["findings"][0]["poc"]["response_evidence"].as_str().unwrap().contains("QGATE-OTHER-USER-EMAIL"));
}

#[tokio::test]
async fn without_logging_in_first_the_same_idor_attack_is_refused_and_unproven() {
    let target = login_and_idor_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(idor_hypothesis(), &[host]).await;
    let (status, body) =
        TestServer::send(server.mutate("/api/redteam/scan", json!({ "base_url": target.uri() }))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["findings"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn fetching_an_unknown_scan_is_not_found() {
    let target = vulnerable_target().await;
    let host = target.uri().trim_start_matches("http://").split(':').next().unwrap().to_string();
    let server = TestServer::start(sqli_hypothesis(), &[host]).await;
    let (status, _) = TestServer::send(server.client.get(server.url("/api/redteam/scans/999"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
