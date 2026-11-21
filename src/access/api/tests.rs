use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::header::{CONTENT_SECURITY_POLICY, COOKIE, LOCATION, SET_COOKIE};
use reqwest::{Client, StatusCode, redirect};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::*;
use crate::access::domain::SECRET_BYTES;
use crate::history::api::{DashboardState, router_guarded, serve_router};
use crate::history::infrastructure::SqliteRunRepository;

struct TestServer {
    addr: SocketAddr,
    client: Client,
    secret: DashboardSecret,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repository = Arc::new(SqliteRunRepository::open(dir.path().join("q.db")).unwrap());
        let secret = DashboardSecret::from_random(&[0x5a; SECRET_BYTES]);
        let access = AccessState::new(secret.clone());
        let app = router_guarded(DashboardState::new(repository), Router::new(), |routes| protect(routes, access));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_router(listener, app, async {
            let _ = stopped.await;
        }));
        let client = Client::builder().redirect(redirect::Policy::none()).build().unwrap();
        Self { addr, client, secret, _stop: stop, _dir: dir }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut request = self.client.get(self.url(path));
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        request.send().await.unwrap()
    }

    fn session_cookie(&self) -> String {
        format!("{SESSION_COOKIE}={}", self.secret.session_key().expose())
    }
}

fn location(response: &reqwest::Response) -> &str {
    response.headers()[LOCATION].to_str().unwrap()
}

#[tokio::test]
async fn pages_and_unknown_paths_without_a_session_redirect_to_login() {
    let server = TestServer::start().await;
    for path in ["/", "/settings", "/assets/app.js"] {
        let response = server.get(path, None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{path}");
        assert_eq!(location(&response), LOGIN_PATH, "{path}");
        assert!(response.text().await.unwrap().is_empty(), "{path}: no data before login");
    }
}

#[tokio::test]
async fn api_without_a_valid_session_is_401_json() {
    let server = TestServer::start().await;
    for cookie in [None, Some("qgate_session=nope"), Some("other=1")] {
        let response = server.get("/api/summary", cookie).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body: Value = response.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("login required"));
    }
    let post = server.client.post(server.url("/api/settings")).send().await.unwrap();
    assert_eq!(post.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_valid_session_cookie_passes_among_other_cookies() {
    let server = TestServer::start().await;
    let cookie = format!("theme=dark; {}; x=y", server.session_cookie());
    let page = server.get("/", Some(&cookie)).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(server.get("/api/summary", Some(&cookie)).await.status(), StatusCode::OK);
    assert_eq!(server.get("/missing", Some(&cookie)).await.status(), StatusCode::NOT_FOUND);
    let raw = format!("{SESSION_COOKIE}={}", server.secret.expose());
    assert_eq!(server.get("/api/summary", Some(&raw)).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn healthz_is_open_and_reveals_nothing() {
    let server = TestServer::start().await;
    let response = server.get(HEALTH_PATH, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn login_link_sets_a_strict_http_only_cookie_and_strips_the_token() {
    let server = TestServer::start().await;
    let response = server.get(&format!("{LOGIN_PATH}?token={}", server.secret.expose()), None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/");
    let cookie = response.headers()[SET_COOKIE].to_str().unwrap().to_string();
    assert!(cookie.starts_with(&format!("{}; ", server.session_cookie())), "{cookie}");
    for attribute in ["HttpOnly", "SameSite=Strict", "Path=/", &format!("Max-Age={SESSION_MAX_AGE_SECS}")] {
        assert!(cookie.contains(attribute), "{cookie} lacks {attribute}");
    }
    assert!(!cookie.contains(server.secret.expose()), "the cookie is not the login token");
    let session = cookie.split(';').next().unwrap();
    assert_eq!(server.get("/", Some(session)).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_or_missing_token_is_a_401_page_that_echoes_nothing() {
    let server = TestServer::start().await;
    let wrong = format!("{}0", &server.secret.expose()[..63]);
    let response = server.get(&format!("{LOGIN_PATH}?token={wrong}"), None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(SET_COOKIE).is_none());
    let csp = response.headers()[CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
    assert!(csp.contains("form-action 'self'"), "login page keeps its own CSP: {csp}");
    let html = response.text().await.unwrap();
    assert!(html.contains("not valid") && !html.contains(&wrong));
    assert!(html.contains("if (false &&"), "no auto-retry after a wrong token");

    let missing = server.get(LOGIN_PATH, None).await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let html = missing.text().await.unwrap();
    assert!(html.contains("docker compose logs qgate") && html.contains("if (true &&"));
}

#[tokio::test]
async fn login_without_token_but_with_a_session_goes_home() {
    let server = TestServer::start().await;
    let response = server.get(LOGIN_PATH, Some(&server.session_cookie())).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/");
}

#[tokio::test]
async fn login_form_accepts_a_pasted_token() {
    let server = TestServer::start().await;
    let post = |body: String| {
        server
            .client
            .post(server.url(LOGIN_PATH))
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
    };
    let good = post(format!("token=+{}+", server.secret.expose())).await.unwrap();
    assert_eq!(good.status(), StatusCode::SEE_OTHER);
    assert!(good.headers()[SET_COOKIE].to_str().unwrap().contains("HttpOnly"));
    let bad = post("token=guess".into()).await.unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    assert!(bad.headers().get(SET_COOKIE).is_none());
    let empty = post("other=1".into()).await.unwrap();
    assert_eq!(empty.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn host_check_still_runs_before_login() {
    let server = TestServer::start().await;
    let response = server
        .client
        .get(server.url(&format!("{LOGIN_PATH}?token={}", server.secret.expose())))
        .header(reqwest::header::HOST, "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
