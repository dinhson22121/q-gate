use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, ORIGIN};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use axum::response::IntoResponse;

use super::handlers::ApiError;
use super::*;
use crate::history::api::{DashboardState, serve_on_with};
use crate::history::infrastructure::SqliteRunRepository;
use crate::policy::application::fakes::{
    FakeGates, FakeHistory, FakeModel, FakeRulebooks, FakeSettings, MemoryProposals,
};
use crate::policy::application::{FindingContext, PolicyDeps, PolicyError};
use crate::policy::domain::{DraftRule, DraftSetting, ExistingRule, ProposalDraft, RuleAction};
use crate::web::{ApiToken, TOKEN_HEADER};

const REPO: &str = "/repos/app";
const HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

struct TestServer {
    addr: SocketAddr,
    client: reqwest::Client,
    settings: Arc<FakeSettings>,
    token: ApiToken,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

fn draft() -> ProposalDraft {
    ProposalDraft {
        summary: "Block failing tests and keep handlers free of SQL".into(),
        settings_changes: vec![
            DraftSetting { path: "agents.tests.on_fail".into(), value_json: "\"block\"".into(), reason: "r".into() },
            DraftSetting { path: "watch.run_repo_checks".into(), value_json: "false".into(), reason: "r".into() },
        ],
        rule_changes: vec![DraftRule {
            action: RuleAction::Add,
            title: "No <script> SQL in handlers".into(),
            description: "Handlers call repositories.".into(),
            ..DraftRule::default()
        }],
        ..ProposalDraft::default()
    }
}

impl TestServer {
    async fn start(model: FakeModel) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repository = Arc::new(SqliteRunRepository::open(dir.path().join("qgate.db")).unwrap());
        let settings = Arc::new(FakeSettings::default());
        let rule = ExistingRule {
            id: "R1".into(),
            title: "Domain is pure".into(),
            description: "d".into(),
            status: "approved".into(),
            scope_globs: vec![],
        };
        let rulebooks = FakeRulebooks {
            rules: std::sync::Mutex::new([(REPO.to_string(), vec![rule])].into()),
            ..FakeRulebooks::default()
        };
        let finding = FindingContext {
            repo_path: REPO.into(),
            file: "src/a.rs".into(),
            line: 3,
            severity: "major".into(),
            category: "security".into(),
            title: "SQL injection".into(),
            explanation: String::new(),
            evidence: String::new(),
        };
        let history = FakeHistory { findings: [((5, 0), finding)].into(), ..FakeHistory::default() };
        let service = PolicyService::new(PolicyDeps {
            model: Arc::new(model),
            settings: settings.clone(),
            rulebooks: Arc::new(rulebooks),
            history: Arc::new(history),
            gates: Arc::new(FakeGates::default()),
            store: Arc::new(MemoryProposals::default()),
            now: chrono::Utc::now,
        });
        let hosts: Vec<String> = HOSTS.iter().map(|h| h.to_string()).collect();
        let token = ApiToken::generate().unwrap();
        let extra = router(PolicyApiState::new(Arc::new(service), hosts.clone(), token.clone()));
        let dashboard = DashboardState::with_allowed_hosts(repository, hosts);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_on_with(listener, dashboard, extra, async {
            let _ = stopped.await;
        }));
        Self { addr, client: reqwest::Client::new(), settings, token, _stop: stop, _dir: dir }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// A same-origin request as the page sends it.
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.url(path))
            .header(CONTENT_TYPE, "application/json")
            .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
            .header(ORIGIN, format!("http://localhost:{}", self.addr.port()))
            .header(TOKEN_HEADER, self.token.expose())
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.client.get(self.url(path))).await
    }
}

#[tokio::test]
async fn page_is_served_with_security_headers() {
    let server = TestServer::start(FakeModel::default()).await;
    let response = server.client.get(server.url("/policy")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(CONTENT_SECURITY_POLICY));
    let html = response.text().await.unwrap();
    assert!(html.contains("X-Qgate-Request\": \"policy\""));
    assert!(html.contains(&format!(r#"<meta name="qgate-token" content="{}">"#, server.token.expose())));
    assert!(html.contains(r#""X-Qgate-Token": TOKEN"#) && !html.contains("__QGATE_TOKEN__"));
    assert!(!html.contains("innerHTML"), "the page never parses strings as HTML");
}

#[tokio::test]
async fn mutations_need_the_csrf_header_token_json_and_an_allowed_origin() {
    let server = TestServer::start(FakeModel::returning(draft())).await;
    let body = json!({ "description": "x" });
    let no_header = server.client.post(server.url("/api/policy/proposals")).json(&body);
    assert_eq!(server.send(no_header).await.0, StatusCode::FORBIDDEN);
    let foreign = server
        .client
        .post(server.url("/api/policy/proposals"))
        .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
        .header(ORIGIN, "https://evil.example")
        .json(&body);
    assert_eq!(server.send(foreign).await.0, StatusCode::FORBIDDEN);
    let text = server
        .client
        .post(server.url("/api/policy/proposals"))
        .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
        .header(TOKEN_HEADER, server.token.expose())
        .header(CONTENT_TYPE, "text/plain")
        .body(body.to_string());
    assert_eq!(server.send(text).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let no_token = server
        .client
        .post(server.url("/api/policy/proposals"))
        .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
        .json(&body);
    assert_eq!(server.send(no_token).await.0, StatusCode::UNAUTHORIZED);
    let wrong_token = server
        .client
        .post(server.url("/api/policy/proposals/1/approve"))
        .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
        .header(TOKEN_HEADER, "not-the-token")
        .json(&json!({}));
    assert_eq!(server.send(wrong_token).await.0, StatusCode::UNAUTHORIZED);
    let approve = server.client.post(server.url("/api/policy/proposals/1/approve")).json(&json!({}));
    assert_eq!(server.send(approve).await.0, StatusCode::FORBIDDEN);
    let host = server.client.get(server.url("/policy")).header("host", "evil.example");
    assert_eq!(server.send(host).await.0, StatusCode::FORBIDDEN, "dashboard host allow-list applies");
}

#[tokio::test]
async fn propose_preview_dry_run_and_approve_happy_path() {
    let server = TestServer::start(FakeModel::returning(draft())).await;
    let (status, repos) = server.get("/api/policy/repos").await;
    assert_eq!((status, repos["repos"].clone()), (StatusCode::OK, json!([REPO])));

    let (status, view) = server
        .send(server.post("/api/policy/proposals").json(&json!({ "description": "Block on tests", "repo": REPO })))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    assert_eq!(view["proposal"]["status"], "draft");
    assert_eq!(view["proposal"]["content"]["settings"][1]["sensitivity"], json!(["repo_checks", "gate_loosening"]));
    assert_eq!(view["proposal"]["content"]["rules"][0]["title"], "No <script> SQL in handlers");
    assert!(view["preview"].as_array().unwrap().len() >= 2);

    let (status, fetched) = server.get("/api/policy/proposals/1").await;
    assert_eq!((status, fetched["proposal"]["id"].clone()), (StatusCode::OK, json!(1)));
    let (status, report) = server.get("/api/policy/proposals/1/dry-run?limit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["evaluated"], 0);
    assert_eq!(report["caveats"].as_array().unwrap().len(), 2);

    let selection = json!({ "settings": [0, 1], "rules": [{ "index": 0, "status": "proposed" }] });
    let (status, body) = server.send(server.post("/api/policy/proposals/1/approve").json(&selection)).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    assert!(body["error"].as_str().unwrap().contains("watch.run_repo_checks"));
    let confirmed =
        json!({ "settings": [0, 1], "rules": [{ "index": 0, "status": "proposed" }], "confirm_sensitive": true });
    let (status, body) = server.send(server.post("/api/policy/proposals/1/approve").json(&confirmed)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["proposal"]["status"], "applied");
    assert_eq!(body["proposal"]["outcome"]["settings_version"], 4);
    assert_eq!(server.settings.applied.lock().unwrap()[0].1, "policy-assistant:p1");
    let (status, _) = server.send(server.post("/api/policy/proposals/1/approve").json(&confirmed)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, list) = server.get("/api/policy/proposals").await;
    assert_eq!((status, list["proposals"][0]["status"].clone()), (StatusCode::OK, json!("applied")));
}

#[tokio::test]
async fn stale_proposals_conflict_until_re_previewed_and_can_be_rejected() {
    let server = TestServer::start(FakeModel::returning(draft())).await;
    server.send(server.post("/api/policy/proposals").json(&json!({ "description": "x" }))).await;
    server.settings.bump_version();
    let (status, body) =
        server.send(server.post("/api/policy/proposals/1/approve").json(&json!({ "settings": [0] }))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("re-preview"));
    let (status, fresh) = server.send(server.post("/api/policy/proposals/1/repreview").json(&json!({}))).await;
    assert_eq!((status, fresh["proposal"]["id"].clone()), (StatusCode::CREATED, json!(2)));
    let (status, rejected) = server.send(server.post("/api/policy/proposals/2/reject").json(&json!({}))).await;
    assert_eq!((status, rejected["proposal"]["status"].clone()), (StatusCode::OK, json!("rejected")));
}

#[tokio::test]
async fn bad_input_and_missing_items_map_to_clear_statuses() {
    let server = TestServer::start(FakeModel::returning(draft())).await;
    let (status, _) = server.send(server.post("/api/policy/proposals").json(&json!({ "desc": "x" }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = server.send(server.post("/api/policy/proposals").json(&json!({ "description": " " }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) =
        server.send(server.post("/api/policy/proposals").json(&json!({ "description": "x", "repo": "/etc" }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(server.get("/api/policy/proposals/abc").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(server.get("/api/policy/proposals/9").await.0, StatusCode::NOT_FOUND);
    let (status, prefill) = server.get("/api/policy/prefill?run=5&finding=0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(prefill["repo"], REPO);
    assert!(prefill["description"].as_str().unwrap().contains("SQL injection at src/a.rs:3"));
    assert_eq!(server.get("/api/policy/prefill?run=5&finding=1").await.0, StatusCode::NOT_FOUND);
    let big = json!({ "description": "x".repeat(70 * 1024) });
    assert_eq!(server.send(server.post("/api/policy/proposals").json(&big)).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn missing_api_key_points_to_settings_and_model_failures_are_generic() {
    let unavailable = FakeModel {
        error: Some(PolicyError::ModelUnavailable("no Anthropic API key configured".into())),
        ..FakeModel::default()
    };
    let server = TestServer::start(unavailable).await;
    let (status, body) = server.send(server.post("/api/policy/proposals").json(&json!({ "description": "x" }))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap().contains("/settings"));
    assert_eq!(server.get("/policy").await.0, StatusCode::OK, "the page works without a key");

    let failing = FakeModel { error: Some(PolicyError::Model("secret detail".into())), ..FakeModel::default() };
    let server = TestServer::start(failing).await;
    let (status, body) = server.send(server.post("/api/policy/proposals").json(&json!({ "description": "x" }))).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!body.to_string().contains("secret detail"));
    let storage = ApiError::from(PolicyError::Storage("disk".into())).into_response();
    assert_eq!(storage.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
