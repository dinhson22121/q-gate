use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, ORIGIN};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::*;
use crate::conventions::application::RulebookStore;
use crate::conventions::application::fakes::{FakeModel, MemoryRepo, MemoryStore};
use crate::conventions::domain::rulebook::{FileLine, Rule, RuleKind, RuleStatus, Rulebook};
use crate::history::api::{DashboardState, serve_on_with};
use crate::history::infrastructure::SqliteRunRepository;
use crate::web::{ApiToken, TOKEN_HEADER};

const REPO: &str = "/repos/api-shop";

struct TestServer {
    addr: SocketAddr,
    client: reqwest::Client,
    store: Arc<MemoryStore>,
    model: Arc<FakeModel>,
    token: ApiToken,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

fn rulebook() -> Rulebook {
    let rule = |id: &str, title: &str, kind, status| Rule {
        id: id.into(),
        title: title.into(),
        description: "<img src=x onerror=alert(1)>".into(),
        kind,
        status,
        scope_globs: Vec::new(),
        examples: vec![FileLine { file: "CLAUDE.md".into(), line: 1, quote: "domain is pure".into() }],
        lint_suggestion: Some("clippy::unwrap_used".into()),
    };
    Rulebook {
        repo: REPO.into(),
        fingerprint: "fp".into(),
        rules: vec![
            rule("R1", "Domain is pure", RuleKind::Explicit, RuleStatus::Approved),
            rule("R2", "Errors use thiserror", RuleKind::Inferred, RuleStatus::Proposed),
        ],
        created_at: Utc::now(),
    }
}

impl TestServer {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runs = Arc::new(SqliteRunRepository::open(dir.path().join("qgate.db")).unwrap());
        let store = Arc::new(MemoryStore::default());
        store.save(&rulebook()).unwrap();
        let model = Arc::new(FakeModel::default());
        let service = Arc::new(RulebookService::new(model.clone(), Arc::new(MemoryRepo::default()), store.clone()));
        let hosts = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let token = ApiToken::generate().unwrap();
        let extra = router(RulebookApiState::new(service, hosts.clone(), token.clone()));
        let dashboard = DashboardState::with_allowed_hosts(runs, hosts);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_on_with(listener, dashboard, extra, async {
            let _ = stopped.await;
        }));
        Self { addr, client: reqwest::Client::new(), store, model, token, _stop: stop, _dir: dir }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.custom(path, "application/json", &format!("http://localhost:{}", self.addr.port()))
    }

    fn custom(&self, path: &str, content_type: &str, origin: &str) -> reqwest::RequestBuilder {
        self.without_token(path, content_type, origin).header(TOKEN_HEADER, self.token.expose())
    }

    fn without_token(&self, path: &str, content_type: &str, origin: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.url(path))
            .header(CONTENT_TYPE, content_type)
            .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
            .header(ORIGIN, origin)
    }

    async fn get_json(&self, path: &str) -> (StatusCode, Value) {
        let response = self.client.get(self.url(path)).send().await.unwrap();
        (response.status(), response.json().await.unwrap_or(Value::Null))
    }
}

fn encoded(repo: &str) -> String {
    repo.replace('/', "%2F")
}

fn decision_path(id: &str) -> String {
    format!("/api/rulebook/{}/rules/{id}/decision", encoded(REPO))
}

#[tokio::test]
async fn page_is_self_contained_and_never_parses_html() {
    let server = TestServer::start().await;
    let response = server.client.get(server.url("/rulebook")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("content-security-policy").is_some(), "dashboard security headers apply");
    let html = response.text().await.unwrap();
    for forbidden in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "https://", "http://", "eval("]
    {
        assert!(!html.contains(forbidden), "page must not contain {forbidden}");
    }
    assert!(html.contains("textContent") && html.contains("X-Qgate-Request"));
    let meta = format!(r#"<meta name="qgate-token" content="{}">"#, server.token.expose());
    assert!(html.contains(&meta), "the page carries this instance's token");
    assert!(html.contains(r#""X-Qgate-Token": TOKEN"#) && !html.contains("__QGATE_TOKEN__"));
    assert!(html.contains("prefers-color-scheme: dark"));
}

#[tokio::test]
async fn lists_repos_and_reads_a_rulebook() {
    let server = TestServer::start().await;
    assert_eq!(server.get_json("/api/rulebook/repos").await, (StatusCode::OK, json!({ "repos": [REPO] })));
    let (status, view) = server.get_json(&format!("/api/rulebook?repo={}", encoded(REPO))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["repo"], REPO);
    assert_eq!(view["building"], false);
    assert_eq!(view["rulebook"]["rules"][1]["status"], "proposed");
    assert_eq!(view["rulebook"]["rules"][0]["kind"], "explicit");
    assert_eq!(server.get_json("/api/rulebook").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(server.get_json("/api/rulebook?repo=%2Fother").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn decision_persists_and_is_validated() {
    let server = TestServer::start().await;
    let response = server.post(&decision_path("R2")).json(&json!({ "status": "approved" })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["rulebook"]["rules"][1]["status"], "approved");
    assert_eq!(server.store.load(REPO).unwrap().unwrap().rules[1].status, RuleStatus::Approved);
    assert_eq!(server.store.decisions(REPO).unwrap()["errors use thiserror"], RuleStatus::Approved);

    let bad = server.post(&decision_path("R2")).json(&json!({ "status": "maybe" })).send().await.unwrap();
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let garbage = server.post(&decision_path("R2")).body("nope").send().await.unwrap();
    assert_eq!(garbage.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let unknown = server.post(&decision_path("R9")).json(&json!({ "status": "approved" })).send().await.unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let huge =
        server.post(&decision_path("R2")).body(format!("\"{}\"", "x".repeat(MAX_BODY_BYTES))).send().await.unwrap();
    assert_eq!(huge.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn mutations_require_csrf_header_allowed_origin_token_and_json() {
    let server = TestServer::start().await;
    let body = json!({ "status": "rejected" }).to_string();
    let url = server.url(&decision_path("R1"));
    let no_header =
        server.client.post(&url).header(CONTENT_TYPE, "application/json").body(body.clone()).send().await.unwrap();
    assert_eq!(no_header.status(), StatusCode::FORBIDDEN);
    let evil = server.custom(&decision_path("R1"), "application/json", "https://evil.example").body(body.clone());
    assert_eq!(evil.send().await.unwrap().status(), StatusCode::FORBIDDEN);
    let origin = format!("http://127.0.0.1:{}", server.addr.port());
    let form = server.custom(&decision_path("R1"), "text/plain", &origin).body(body.clone());
    assert_eq!(form.send().await.unwrap().status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let rebuild = server.client.post(server.url(&format!("/api/rulebook/{}/rebuild", encoded(REPO))));
    assert_eq!(rebuild.send().await.unwrap().status(), StatusCode::FORBIDDEN);
    let no_token = server.without_token(&decision_path("R1"), "application/json", &origin).body(body.clone());
    assert_eq!(no_token.send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let wrong =
        server.without_token(&decision_path("R1"), "application/json", &origin).header(TOKEN_HEADER, "0".repeat(64));
    let wrong = wrong.body(body.clone()).send().await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(wrong.json::<Value>().await.unwrap()["error"].as_str().unwrap().contains("reload the page"));
    assert_eq!(server.store.load(REPO).unwrap().unwrap().rules[0].status, RuleStatus::Approved);
}

#[tokio::test]
async fn rebuild_runs_in_background_for_known_repos_only() {
    let server = TestServer::start().await;
    let response = server.post(&format!("/api/rulebook/{}/rebuild", encoded(REPO))).body("{}").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    for _ in 0..100 {
        if server.model.draft_calls() == 1
            && !server.get_json(&format!("/api/rulebook?repo={}", encoded(REPO))).await.1["building"]
                .as_bool()
                .unwrap_or(true)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.model.draft_calls(), 1);
    assert!(server.store.load(REPO).unwrap().unwrap().rules.is_empty());

    let unknown = server.post(&format!("/api/rulebook/{}/rebuild", encoded("/etc"))).body("{}").send().await.unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}
