use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, X_CONTENT_TYPE_OPTIONS};
use serde_json::Value;
use tokio::sync::oneshot;

use super::*;
use crate::history::application::RecordRun;
use crate::history::domain::ReviewRun;
use crate::history::domain::fixtures::live_run;
use crate::history::infrastructure::SqliteRunRepository;

struct TestServer {
    addr: SocketAddr,
    run_id: i64,
    repository: Arc<dyn RunRepository>,
    client: reqwest::Client,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start(allowed_hosts: Option<Vec<String>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repository: Arc<dyn RunRepository> = Arc::new(SqliteRunRepository::open(dir.path().join("q.db")).unwrap());
        let run_id = RecordRun::new(repository.clone()).execute(&live_run("api", chrono::Utc::now())).unwrap();
        let state = match allowed_hosts {
            Some(hosts) => DashboardState::with_allowed_hosts(repository.clone(), hosts),
            None => DashboardState::new(repository.clone()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_on(listener, state, async {
            let _ = stopped.await;
        }));
        Self { addr, run_id, repository, client: reqwest::Client::new(), _stop: stop, _dir: dir }
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client.get(format!("http://{}{path}", self.addr)).send().await.unwrap()
    }

    async fn get_with_host(&self, path: &str, host: &str) -> reqwest::Response {
        self.client.get(format!("http://{}{path}", self.addr)).header(HOST, host).send().await.unwrap()
    }

    async fn json(&self, path: &str) -> (StatusCode, Value) {
        let response = self.get(path).await;
        (response.status(), response.json().await.unwrap())
    }
}

#[tokio::test]
async fn summary_has_both_periods_and_repos() {
    let server = TestServer::start(None).await;
    let (status, body) = server.json("/api/summary").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["last_7_days"]["runs"], 1);
    assert_eq!(body["last_7_days"]["failed"], 1);
    assert_eq!(body["last_7_days"]["pass_rate"], 0.0);
    assert_eq!(body["last_7_days"]["findings_by_severity"]["critical"], 1);
    assert_eq!(body["last_7_days"]["findings_by_category"]["security"], 1);
    assert_eq!(body["last_7_days"]["criteria"]["not_met"], 1);
    assert_eq!(body["last_7_days"]["failed_checks"], 1);
    assert_eq!(body["last_30_days"]["runs"], 1);
    assert_eq!(body["repos"], serde_json::json!(["api"]));
}

#[tokio::test]
async fn runs_list_supports_filters_and_validates_params() {
    let server = TestServer::start(None).await;
    let (status, body) = server.json("/api/runs?repo=api&status=failed&limit=500&offset=0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["limit"], 200);
    let item = &body["items"][0];
    assert_eq!(item["id"], server.run_id);
    assert_eq!(item["status"], "failed");
    assert_eq!(item["trigger"], "manual");
    assert_eq!(item["findings"]["critical"], 1);
    assert_eq!((item["criteria_met"].as_u64(), item["criteria_total"].as_u64()), (Some(0), Some(1)));
    assert!(item.get("report").is_none());

    let (_, body) = server.json("/api/runs?status=passed").await;
    assert_eq!(body["total"], 0);
    assert_eq!(body["items"], serde_json::json!([]));

    for bad in ["/api/runs?status=bogus", "/api/runs?limit=-1", "/api/runs?offset=x"] {
        let (status, body) = server.json(bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(body["error"].is_string());
    }
}

#[tokio::test]
async fn run_detail_returns_full_report_or_404() {
    let server = TestServer::start(None).await;
    let (status, body) = server.json(&format!("/api/runs/{}", server.run_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["repo_name"], "api");
    assert_eq!(body["report"]["gate"]["passed"], false);
    assert_eq!(body["report"]["requirements"]["assessments"][0]["status"], "not_met");
    assert_eq!(body["report"]["checks"][0]["output_tail"], "error: unused");
    assert_eq!(body["report"]["findings"][0]["file"], "src/<script>.rs");
    assert_eq!(body["report"]["rejected"][0]["reason"], "evidence_not_found");
    assert_eq!(body["report"]["skipped"][0]["reason"]["kind"], "binary");

    let (status, body) = server.json("/api/runs/999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not found");
    assert_eq!(server.json("/api/runs/abc").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(server.json("/api/nope").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn recurring_endpoint_returns_a_list() {
    let server = TestServer::start(None).await;
    let (status, body) = server.json("/api/recurring").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn rejects_foreign_host_headers() {
    let server = TestServer::start(None).await;
    for host in ["evil.com", "evil.com:8080", "localhost.evil.com", "127.0.0.1.nip.io:80"] {
        let response = server.get_with_host("/api/summary", host).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{host}");
        assert!(response.headers().contains_key(CONTENT_SECURITY_POLICY));
    }
    assert_eq!(server.get_with_host("/", "evil.com").await.status(), StatusCode::FORBIDDEN);
    for host in ["localhost:8080", "LOCALHOST", "127.0.0.1:1"] {
        assert_eq!(server.get_with_host("/api/summary", host).await.status(), StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn allowed_hosts_are_configurable() {
    let server = TestServer::start(Some(vec!["qgate.internal".into(), "[::1]".into()])).await;
    assert_eq!(server.get_with_host("/api/summary", "qgate.internal:8080").await.status(), StatusCode::OK);
    assert_eq!(server.get_with_host("/api/summary", "[::1]:8080").await.status(), StatusCode::OK);
    assert_eq!(server.get_with_host("/api/summary", "localhost:8080").await.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn sets_security_headers_and_no_cors() {
    let server = TestServer::start(None).await;
    for path in ["/", "/api/summary", "/api/runs/999"] {
        let response = server.get(path).await;
        let headers = response.headers();
        assert_eq!(
            headers[CONTENT_SECURITY_POLICY],
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        );
        assert_eq!(headers[X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(!headers.contains_key("access-control-allow-origin"), "{path}");
    }
}

#[tokio::test]
async fn serves_a_self_contained_dashboard() {
    let server = TestServer::start(None).await;
    let response = server.get("/").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()[CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    let html = response.text().await.unwrap();
    assert!(html.contains("Acceptance criteria") && html.contains("Recurring issues"));
    for section in ["Generated tests", "Architecture", "Design", "Agent notes", "/api/tickets/", "/rulebook?repo="] {
        assert!(html.contains(section), "dashboard lacks {section}");
    }
}

#[tokio::test]
async fn ticket_endpoint_lists_matching_runs_newest_first_with_criterion_progress() {
    let server = TestServer::start(None).await;
    let record = RecordRun::new(server.repository.clone());
    let older = record.execute(&live_run("web", chrono::Utc::now() - chrono::Duration::days(1))).unwrap();
    let mut other = live_run("api", chrono::Utc::now());
    other.ticket_key = Some("QG-70".into());
    other.report.as_mut().unwrap().requirements.as_mut().unwrap().requirements.key = "QG-70".into();
    record.execute(&other).unwrap();

    let (status, body) = server.json("/api/tickets/QG-7").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["key"], "QG-7");
    let ids: Vec<i64> = body["runs"].as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, vec![server.run_id, older]);
    assert_eq!(body["criteria"][0]["id"], "AC1");
    assert_eq!(body["criteria"][0]["text"], "Users can log in");
    assert_eq!(body["criteria"][0]["latest"]["status"], "not_met");
    assert_eq!(body["criteria"][0]["latest"]["run_id"], server.run_id);
    assert_eq!(body["criteria"][0]["history"].as_array().unwrap().len(), 2);

    let (status, body) = server.json("/api/tickets/SHOP-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((body["runs"].clone(), body["criteria"].clone()), (serde_json::json!([]), serde_json::json!([])));
}

#[tokio::test]
async fn ticket_endpoint_rejects_malformed_keys() {
    let server = TestServer::start(None).await;
    for bad in ["qg-7", "QG-07", "QG-7%20OR%201=1", "%22QG-7%22", "QG-7%2FAC1", "Q-1", "ABCDEFGHIJK-1"] {
        let (status, body) = server.json(&format!("/api/tickets/{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(body["error"].as_str().unwrap().contains("ticket key"), "{bad}");
    }
}

#[test]
fn dashboard_html_has_no_external_resources_or_html_sinks() {
    let html = include_str!("assets/dashboard.html");
    for forbidden in [
        "://",
        "@import",
        "<link",
        "src=",
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "setTimeout(\"",
        "javascript:",
    ] {
        assert!(!html.contains(forbidden), "dashboard must not contain {forbidden}");
    }
}

#[test]
fn normalizes_host_header_values() {
    assert_eq!(security::normalize_host("LocalHost:8080"), "localhost");
    assert_eq!(security::normalize_host("[::1]:9"), "::1");
    assert_eq!(security::normalize_host("::1"), "::1");
    assert_eq!(security::normalize_host("example.com."), "example.com");
}

#[tokio::test]
async fn exports_a_run_as_markdown_or_404s() {
    let server = TestServer::start(None).await;
    let md = "# Review — PASSED\n\nNo blocking findings.\n";
    let run = ReviewRun { report_md: Some(md.to_string()), ..live_run("api", chrono::Utc::now()) };
    let id = server.repository.insert(&run).unwrap();

    let response = server.get(&format!("/api/runs/{id}/export")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers().get(CONTENT_TYPE).unwrap().to_str().unwrap().to_string();
    assert!(content_type.starts_with("text/markdown"), "{content_type}");
    let disposition =
        response.headers().get(reqwest::header::CONTENT_DISPOSITION).unwrap().to_str().unwrap().to_string();
    assert!(disposition.starts_with("attachment;") && disposition.contains(".md"), "{disposition}");
    assert_eq!(response.text().await.unwrap(), md);

    assert_eq!(server.get(&format!("/api/runs/{}/export", server.run_id)).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(server.get("/api/runs/99999/export").await.status(), StatusCode::NOT_FOUND);
    assert_eq!(server.get("/api/runs/abc/export").await.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn index_injects_the_token_only_when_the_state_has_one() {
    let dir = tempfile::tempdir().unwrap();
    let repository: Arc<dyn RunRepository> = Arc::new(SqliteRunRepository::open(dir.path().join("q.db")).unwrap());
    let token = crate::web::ApiToken::generate().unwrap();
    let app = router(DashboardState::new(repository.clone()).with_token(token.clone()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    let html = reqwest::get(format!("http://{addr}/")).await.unwrap().text().await.unwrap();
    assert!(html.contains(&format!(r#"name="qgate-token" content="{}""#, token.expose())));
    assert!(!html.contains("__QGATE_TOKEN__"));
}
