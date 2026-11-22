use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, ORIGIN};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::*;
use crate::history::api::{DashboardState, serve_on_with};
use crate::history::infrastructure::SqliteRunRepository;
use crate::watch::application::manage_repos::MAX_IMPORTED_REPOS;
use crate::watch::application::ports::RepoSource;
use crate::watch::infrastructure::git::test_support::init_repo;
use crate::watch::infrastructure::toml_registry::TomlRepoRegistry;
use crate::web::{REQUEST_HEADER, TOKEN_HEADER};

struct RecordingScanner(Arc<std::sync::Mutex<Vec<PathBuf>>>);
impl crate::watch::application::ports::ManualScanner for RecordingScanner {
    fn scan(&self, repo_root: PathBuf) {
        self.0.lock().unwrap().push(repo_root);
    }
}

struct TestServer {
    addr: SocketAddr,
    client: reqwest::Client,
    registry: TomlRepoRegistry,
    base: PathBuf,
    token: ApiToken,
    scans: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap().join("repos");
        for name in ["one", "two"] {
            std::fs::create_dir_all(base.join(name)).unwrap();
            init_repo(&base.join(name));
        }
        std::fs::create_dir_all(base.join("plain")).unwrap();
        let registry = TomlRepoRegistry::new(dir.path().join("config").join("repos.toml"));
        let runs = Arc::new(SqliteRunRepository::open(dir.path().join("qgate.db")).unwrap());
        let hosts = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let token = ApiToken::generate().unwrap();
        let scans = Arc::new(std::sync::Mutex::new(Vec::new()));
        let extra = router(
            ReposApiState::new(registry.clone(), base.clone(), hosts.clone(), token.clone())
                .with_scanner(Arc::new(RecordingScanner(scans.clone()))),
        );
        let dashboard = DashboardState::with_allowed_hosts(runs, hosts);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_on_with(listener, dashboard, extra, async {
            let _ = stopped.await;
        }));
        Self { addr, client: reqwest::Client::new(), registry, base, token, scans, _stop: stop, _dir: dir }
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

    fn mutate(&self, method: reqwest::Method, path: &str, body: Value) -> reqwest::RequestBuilder {
        self.page_request(method, path).header(TOKEN_HEADER, self.token.expose()).body(body.to_string())
    }

    async fn send(request: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let response = request.send().await.unwrap();
        (response.status(), response.json().await.unwrap_or(Value::Null))
    }

    async fn import(&self, name: &str) -> (StatusCode, Value) {
        let path = self.base.join(name);
        Self::send(self.mutate(reqwest::Method::POST, "/api/repos/import", json!({ "path": path }))).await
    }

    async fn get_json(&self, path: &str) -> (StatusCode, Value) {
        Self::send(self.client.get(self.url(path))).await
    }
}

#[tokio::test]
async fn page_is_self_contained_and_carries_the_token() {
    let server = TestServer::start().await;
    let response = server.client.get(server.url("/repos")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("content-security-policy").is_some());
    let html = response.text().await.unwrap();
    for forbidden in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "https://", "http://", "eval("]
    {
        assert!(!html.contains(forbidden), "page must not contain {forbidden}");
    }
    assert!(html.contains(&format!(r#"<meta name="qgate-token" content="{}">"#, server.token.expose())));
    assert!(html.contains(r#""X-Qgate-Request": "repos""#) && html.contains(r#""X-Qgate-Token": TOKEN"#));
    assert!(!html.contains("__QGATE_TOKEN__"));
}

#[tokio::test]
async fn browse_lists_folders_and_flags_git_roots() {
    let server = TestServer::start().await;
    let (status, body) = server.get_json("/api/repos/browse").await;
    assert_eq!(status, StatusCode::OK);
    let entries: Vec<_> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["is_git_root"].as_bool().unwrap()))
        .collect();
    assert_eq!(entries, vec![("one", true), ("plain", false), ("two", true)]);
    assert_eq!(body["base"], json!(server.base));
    assert!(body["parent"].is_null());
}

#[tokio::test]
async fn browse_refuses_to_leave_the_base() {
    let server = TestServer::start().await;
    for path in ["..", "%2e%2e%2f", "%2Fetc"] {
        let (status, _) = server.get_json(&format!("/api/repos/browse?path={path}")).await;
        assert!(matches!(status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND), "{path}: {status}");
    }
    let (status, _) = server.get_json("/api/repos/browse?path=..").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = server.get_json("/api/repos/browse?path=%2Fetc").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = server.get_json("/api/repos/browse?path=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn import_is_additive_idempotent_and_removal_keeps_the_others() {
    let server = TestServer::start().await;
    let (status, body) = server.import("one").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["repo"]["name"], "one");
    let (status, body) = server.import("two").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["repo"]["name"], "two");
    assert_eq!(server.import("one").await.0, StatusCode::CREATED);

    let (status, body) = server.get_json("/api/repos").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["max_repos"], MAX_IMPORTED_REPOS);
    let repos = body["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 2, "both repositories are watched, re-import adds no duplicate");
    assert_eq!(repos[0]["path"], json!(server.base.join("one")));
    assert_eq!(repos[1]["path"], json!(server.base.join("two")));
    assert_eq!(repos[0]["exists"], true);
    assert_eq!(server.registry.list().await.unwrap().len(), 2, "persisted in repos.toml");

    let path = json!({ "path": server.base.join("one") });
    let removed = TestServer::send(server.mutate(reqwest::Method::DELETE, "/api/repos", path)).await;
    assert_eq!(removed.0, StatusCode::OK);
    let remaining = server.registry.list().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].path, server.base.join("two"));
}

#[tokio::test]
async fn import_validates_the_path() {
    let server = TestServer::start().await;
    let post = |path: Value| {
        TestServer::send(server.mutate(reqwest::Method::POST, "/api/repos/import", json!({ "path": path })))
    };
    assert_eq!(post(json!(server.base.join("plain"))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(post(json!("/etc")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post(json!("../x")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post(json!("missing")).await.0, StatusCode::NOT_FOUND);
    let bad_body = server.mutate(reqwest::Method::POST, "/api/repos/import", json!({ "nope": 1 }));
    assert_eq!(TestServer::send(bad_body).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(server.registry.list().await.unwrap().is_empty(), "nothing persisted on failure");
}

#[tokio::test]
async fn remove_unregisters_and_reports_unknown() {
    let server = TestServer::start().await;
    server.import("one").await;
    let path = json!({ "path": server.base.join("one") });
    let (status, body) = TestServer::send(server.mutate(reqwest::Method::DELETE, "/api/repos", path.clone())).await;
    assert_eq!((status, &body["removed"]), (StatusCode::OK, &json!(true)));
    let (status, _) = TestServer::send(server.mutate(reqwest::Method::DELETE, "/api/repos", path)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(server.registry.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn mutations_require_header_host_and_token() {
    let server = TestServer::start().await;
    let path = json!({ "path": server.base.join("one") }).to_string();
    let send = |request: reqwest::RequestBuilder| async move { request.send().await.unwrap().status() };

    let no_token = server.page_request(reqwest::Method::POST, "/api/repos/import").body(path.clone());
    assert_eq!(send(no_token).await, StatusCode::UNAUTHORIZED);
    let wrong_token =
        server.page_request(reqwest::Method::POST, "/api/repos/import").header(TOKEN_HEADER, "00").body(path.clone());
    assert_eq!(send(wrong_token).await, StatusCode::UNAUTHORIZED);
    let wrong_header = server
        .client
        .delete(server.url("/api/repos"))
        .header(CONTENT_TYPE, "application/json")
        .header(REQUEST_HEADER, "rulebook")
        .header(TOKEN_HEADER, server.token.expose())
        .body(r#"{"path":"x"}"#);
    assert_eq!(send(wrong_header).await, StatusCode::FORBIDDEN);
    let hostile = |header: &'static str, value: &'static str| {
        server
            .client
            .post(server.url("/api/repos/import"))
            .header(CONTENT_TYPE, "application/json")
            .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
            .header(TOKEN_HEADER, server.token.expose())
            .header(header, value)
            .body(r#"{"path":"one"}"#)
    };
    assert_eq!(send(hostile("origin", "http://evil.example")).await, StatusCode::FORBIDDEN);
    assert_eq!(send(hostile("host", "evil.example")).await, StatusCode::FORBIDDEN);
    assert!(server.registry.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn scan_triggers_only_for_imported_repos() {
    let server = TestServer::start().await;
    let path = server.base.join("one");
    let body = json!({ "path": path });

    let (status, _) = TestServer::send(server.mutate(reqwest::Method::POST, "/api/repos/scan", body.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a repository that is not imported cannot be scanned");
    assert!(server.scans.lock().unwrap().is_empty());

    server.import("one").await;
    let (status, reply) = TestServer::send(server.mutate(reqwest::Method::POST, "/api/repos/scan", body)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(reply["started"], json!(true));
    assert_eq!(server.scans.lock().unwrap().as_slice(), &[path]);
}
