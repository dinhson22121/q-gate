use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, ORIGIN};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::handlers::facts_of;
use super::*;
use crate::feedback::infrastructure::SqliteFeedbackStore;
use crate::history::api::{DashboardState, serve_on_with};
use crate::history::application::{FindingView, RecordRun};
use crate::history::domain::fixtures::live_run;
use crate::history::infrastructure::SqliteRunRepository;
use crate::review::application::{Finding, finding_identity};
use crate::review::domain::finding::FindingSource;
use crate::web::TOKEN_HEADER;

struct TestServer {
    addr: SocketAddr,
    run_id: i64,
    repo: String,
    token: ApiToken,
    client: reqwest::Client,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runs: Arc<dyn RunRepository> = Arc::new(SqliteRunRepository::open(dir.path().join("q.db")).unwrap());
        let run = live_run("api", chrono::Utc::now());
        let run_id = RecordRun::new(runs.clone()).execute(&run).unwrap();
        let store = Arc::new(SqliteFeedbackStore::open(dir.path().join("f.db")).unwrap());
        let hosts = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let token = ApiToken::generate().unwrap();
        let extra = router(FeedbackApiState::new(
            Arc::new(FeedbackService::new(store)),
            runs.clone(),
            hosts.clone(),
            token.clone(),
        ));
        let dashboard = DashboardState::with_allowed_hosts(runs, hosts);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_on_with(listener, dashboard, extra, async {
            let _ = stopped.await;
        }));
        Self { addr, run_id, repo: run.repo_path, token, client: reqwest::Client::new(), _stop: stop, _dir: dir }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn request_with(&self, path: &str, content_type: &str, origin: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.url(path))
            .header(CONTENT_TYPE, content_type)
            .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
            .header(ORIGIN, origin)
            .header(TOKEN_HEADER, self.token.expose())
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.request_with(path, "application/json", &format!("http://localhost:{}", self.addr.port()))
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self.request(path).body(body.to_string()).send().await.unwrap();
        (response.status(), response.json().await.unwrap_or(Value::Null))
    }

    async fn feedback(&self) -> Value {
        let response = self.client.get(self.url(&format!("/api/feedback/runs/{}", self.run_id))).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }
}

fn view_of(finding: &Finding) -> FindingView {
    serde_json::from_value(serde_json::to_value(finding).unwrap()).unwrap()
}

#[test]
fn facts_from_a_stored_view_match_the_live_finding_identity() {
    let live = crate::review::domain::finding::fixtures::finding(
        "a.go",
        3,
        crate::review::domain::finding::Severity::Major,
        "ev",
    );
    let tool = Finding { source: FindingSource::StaticTool("gitleaks".into()), ..live.clone() };
    for finding in [live, tool] {
        let identity = finding_identity(&finding);
        let facts = facts_of(&view_of(&finding));
        assert_eq!(
            (facts.file, facts.category, facts.source, facts.evidence),
            (identity.file, identity.category, identity.source, identity.evidence)
        );
    }
}

#[test]
fn facts_default_to_llm_when_an_old_run_has_no_source() {
    let live = crate::review::domain::finding::fixtures::finding(
        "a.go",
        3,
        crate::review::domain::finding::Severity::Major,
        "ev",
    );
    let mut value = serde_json::to_value(&live).unwrap();
    value.as_object_mut().unwrap().remove("source");
    let view: FindingView = serde_json::from_value(value).unwrap();
    assert_eq!(facts_of(&view).source, "llm");
}

#[tokio::test]
async fn lists_items_for_every_finding_without_verdicts_at_first() {
    let server = TestServer::start().await;
    let body = server.feedback().await;
    assert_eq!(body["repo"], server.repo.as_str());
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["index"], 0);
    assert_eq!(body["items"][0]["kind"], "llm/security");
    assert_eq!(body["items"][0]["verdict"], Value::Null);
    assert!(body["items"][0]["fingerprint"].as_str().is_some_and(|f| !f.is_empty()));
    assert!(body["scores"].is_array());
}

#[tokio::test]
async fn records_then_undoes_a_verdict() {
    let server = TestServer::start().await;
    let (status, body) =
        server.post("/api/feedback", json!({"run_id": server.run_id, "index": 0, "verdict": "dismissed"})).await;
    assert_eq!((status, &body["verdict"]), (StatusCode::OK, &json!("dismissed")));
    let fingerprint = body["fingerprint"].as_str().unwrap().to_string();

    let listed = server.feedback().await;
    assert_eq!(listed["items"][0]["verdict"], "dismissed");
    assert_eq!(listed["items"][0]["fingerprint"], fingerprint.as_str());
    assert_eq!(listed["scores"][0]["kind"], "llm/security");

    let (_, undone) = server.post("/api/feedback/undo", json!({"run_id": server.run_id, "index": 0})).await;
    assert_eq!(undone, json!({"removed": true}));
    let (_, again) = server.post("/api/feedback/undo", json!({"run_id": server.run_id, "index": 0})).await;
    assert_eq!(again, json!({"removed": false}));
    assert_eq!(server.feedback().await["items"][0]["verdict"], Value::Null);
}

#[tokio::test]
async fn rejects_unknown_runs_indexes_and_bad_bodies() {
    let server = TestServer::start().await;
    let run_id = server.run_id;
    let missing = server.post("/api/feedback", json!({"run_id": run_id + 99, "index": 0, "verdict": "accepted"})).await;
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
    let range = server.post("/api/feedback", json!({"run_id": run_id, "index": 5, "verdict": "accepted"})).await;
    assert_eq!(range.0, StatusCode::NOT_FOUND);
    let verdict = server.post("/api/feedback", json!({"run_id": run_id, "index": 0, "verdict": "maybe"})).await;
    assert_eq!(verdict.0, StatusCode::BAD_REQUEST);
    let extra =
        server.post("/api/feedback", json!({"run_id": run_id, "index": 0, "verdict": "accepted", "x": 1})).await;
    assert_eq!(extra.0, StatusCode::UNPROCESSABLE_ENTITY);
    let negative = server.post("/api/feedback", json!({"run_id": run_id, "index": -1, "verdict": "accepted"})).await;
    assert_eq!(negative.0, StatusCode::UNPROCESSABLE_ENTITY);
    let get = server.client.get(server.url("/api/feedback/runs/abc")).send().await.unwrap();
    assert_eq!(get.status(), StatusCode::BAD_REQUEST);
    let unknown = server.client.get(server.url("/api/feedback/runs/999")).send().await.unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reset_validates_the_kind_and_the_run() {
    let server = TestServer::start().await;
    let run_id = server.run_id;
    let (status, body) = server.post("/api/feedback/reset", json!({"run_id": run_id, "kind": "llm/security"})).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"reset": true})));
    let blank = server.post("/api/feedback/reset", json!({"run_id": run_id, "kind": "  "})).await;
    assert_eq!(blank.0, StatusCode::BAD_REQUEST);
    let long = server.post("/api/feedback/reset", json!({"run_id": run_id, "kind": "k".repeat(201)})).await;
    assert_eq!(long.0, StatusCode::BAD_REQUEST);
    let missing = server.post("/api/feedback/reset", json!({"run_id": run_id + 9, "kind": "llm/security"})).await;
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mutations_require_header_token_origin_and_json() {
    let server = TestServer::start().await;
    let body = json!({"run_id": server.run_id, "index": 0, "verdict": "accepted"}).to_string();
    let url = server.url("/api/feedback");
    let plain = server.client.post(&url).header(CONTENT_TYPE, "application/json").body(body.clone());
    assert_eq!(plain.send().await.unwrap().status(), StatusCode::FORBIDDEN);
    let no_token =
        server.client.post(&url).header(CONTENT_TYPE, "application/json").header(REQUEST_HEADER, REQUEST_HEADER_VALUE);
    assert_eq!(no_token.body(body.clone()).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let foreign = server.request_with("/api/feedback", "application/json", "http://evil.example").body(body.clone());
    assert_eq!(foreign.send().await.unwrap().status(), StatusCode::FORBIDDEN);
    let text = server.request_with("/api/feedback", "text/plain", "http://localhost").body(body);
    assert_eq!(text.send().await.unwrap().status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
