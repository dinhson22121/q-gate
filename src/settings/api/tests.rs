use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use reqwest::StatusCode;
use reqwest::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, ORIGIN};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::*;
use crate::history::api::{DashboardState, serve_on_with};
use crate::history::infrastructure::SqliteRunRepository;
use crate::settings::application::credentials::fakes::MemoryStore;
use crate::settings::application::server_settings::fakes::{FixedSeed, env};
use crate::settings::application::{CredentialName, CredentialStore, LoadEffectiveSettings, Secret, ServerConfigStore};
use crate::settings::infrastructure::SqliteServerConfigStore;
use crate::web::{ApiToken, TOKEN_HEADER};

const WEBHOOK: &str = "https://hooks.slack.com/services/T0/B0/webhook-token-xyz";
const SECRET_VALUE: &str = "sk-ant-super-secret-value";

struct TestServer {
    addr: SocketAddr,
    client: reqwest::Client,
    store: Arc<SqliteServerConfigStore>,
    credentials: Arc<MemoryStore>,
    token: ApiToken,
    _stop: oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start(pairs: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("qgate.db");
        let repository = Arc::new(SqliteRunRepository::open(&db).unwrap());
        let store = Arc::new(SqliteServerConfigStore::open(&db).unwrap());
        let mut seed = crate::settings::domain::ServerConfig::default();
        seed.notify.webhook_url = WEBHOOK.into();
        let environment = env(pairs);
        let effective = LoadEffectiveSettings::new(store.clone(), Arc::new(FixedSeed(Some(seed))), environment.clone())
            .execute()
            .unwrap();
        let credentials = Arc::new(MemoryStore::default());
        credentials.set(CredentialName::JiraApiToken, &Secret::new(SECRET_VALUE)).unwrap();
        let hosts = effective.dashboard.allowed_hosts.clone();
        let shared = Arc::new(RwLock::new(effective));
        let deps = Arc::new(SettingsDeps::new(store.clone(), credentials.clone(), shared, environment));
        let token = ApiToken::generate().unwrap();
        let extra = router(SettingsApiState::new(deps, hosts.clone(), token.clone()));
        let dashboard = DashboardState::with_allowed_hosts(repository, hosts);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve_on_with(listener, dashboard, extra, async {
            let _ = stopped.await;
        }));
        Self { addr, client: reqwest::Client::new(), store, credentials, token, _stop: stop, _dir: dir }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// A same-origin request as the page sends it.
    fn page(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.custom(method, path, "application/json", &format!("http://localhost:{}", self.addr.port()))
    }

    fn custom(&self, method: reqwest::Method, path: &str, content_type: &str, origin: &str) -> reqwest::RequestBuilder {
        self.without_token(method, path, content_type, origin).header(TOKEN_HEADER, self.token.expose())
    }

    fn without_token(
        &self,
        method: reqwest::Method,
        path: &str,
        content_type: &str,
        origin: &str,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(method, self.url(path))
            .header(CONTENT_TYPE, content_type)
            .header(REQUEST_HEADER, REQUEST_HEADER_VALUE)
            .header(ORIGIN, origin)
    }

    async fn settings(&self) -> Value {
        self.client.get(self.url("/api/settings")).send().await.unwrap().json().await.unwrap()
    }

    async fn put_config(&self, body: Value) -> (StatusCode, String) {
        let response = self.page(reqwest::Method::PUT, "/api/settings").json(&body).send().await.unwrap();
        (response.status(), response.text().await.unwrap())
    }
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn get_returns_view_without_secret_or_webhook_values() {
    let server = TestServer::start(&[("JIRA_EMAIL", "bot@acme.com")]).await;
    let response = server.client.get(server.url("/api/settings")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(CONTENT_SECURITY_POLICY), "dashboard security headers apply");
    let text = response.text().await.unwrap();
    assert!(!text.contains("webhook-token") && !text.contains(SECRET_VALUE), "{text}");
    let body = parse(&text);
    assert_eq!(body["config"]["notify"]["webhook_url"], "");
    assert_eq!(body["webhook_configured"], true);
    assert_eq!(body["config"]["jira"]["email"], "bot@acme.com");
    assert_eq!(body["locked"], json!([{ "field": "jira.email", "env_var": "JIRA_EMAIL" }]));
    assert_eq!(body["restart_required_fields"].as_array().unwrap().len(), 9);
    assert_eq!(
        body["secrets"][1],
        json!({ "name": "jira_api_token", "configured": true, "source": "store", "env_var": "JIRA_API_TOKEN" })
    );
    assert_eq!(body["config"]["agents"]["tests"]["languages"], json!(["go", "java", "python", "rust"]));
}

#[tokio::test]
async fn put_saves_a_new_version_and_returns_restart_list() {
    let server = TestServer::start(&[]).await;
    let mut config = server.settings().await["config"].clone();
    config["agents"]["max_parallel"] = json!(5);
    config["watch"]["poll_interval_secs"] = json!(10);
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body = parse(&text);
    assert_eq!(body["applied"]["version"], 2);
    assert_eq!(body["applied"]["restart_required"], json!(["watch.poll_interval_secs"]));
    assert_eq!(body["settings"]["config"]["agents"]["max_parallel"], 5);
    assert!(!text.contains("webhook-token"));
    assert_eq!(server.store.load_raw().unwrap().unwrap().notify.webhook_url, WEBHOOK, "webhook kept");

    let history: Value =
        server.client.get(server.url("/api/settings/history")).send().await.unwrap().json().await.unwrap();
    assert_eq!(history[0]["version"], 2);
    assert_eq!(history[0]["changed_fields"], json!(["agents.max_parallel", "watch.poll_interval_secs"]));
    assert_eq!(history[1]["changed_by"], "import:qgate.toml");
    assert!(!history.to_string().contains("webhook-token"));
}

#[tokio::test]
async fn invalid_value_is_422_with_field_and_locked_field_is_409() {
    let server = TestServer::start(&[("QGATE_BIND", "0.0.0.0:8080")]).await;
    let mut config = server.settings().await["config"].clone();
    config["figma"]["block_on"] = json!(["states"]);
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(parse(&text)["field"], "figma.block_on");

    let mut config = server.settings().await["config"].clone();
    config["dashboard"]["bind"] = json!("127.0.0.1:1");
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let body = parse(&text);
    assert_eq!(body["field"], "dashboard.bind");
    assert!(body["error"].as_str().unwrap().contains("QGATE_BIND"));

    let (status, text) = server.put_config(json!({ "config": { "nope": 1 } })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");

    let config = server.settings().await["config"].clone();
    let (status, text) =
        server.put_config(json!({ "config": config, "webhook": { "action": "set", "url": "bad-token-123" } })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "setting a webhook is admin-only");
    assert_eq!(parse(&text)["field"], "notify.webhook_url");
    assert!(!text.contains("bad-token-123"));
}

#[tokio::test]
async fn mutations_require_header_token_json_and_allowed_origin() {
    let server = TestServer::start(&[]).await;
    let config = server.settings().await["config"].clone();
    let body = json!({ "config": config }).to_string();

    let no_header = server
        .client
        .put(server.url("/api/settings"))
        .header(CONTENT_TYPE, "application/json")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(no_header.status(), StatusCode::FORBIDDEN);

    let put = reqwest::Method::PUT;
    let foreign = server.custom(put.clone(), "/api/settings", "application/json", "http://evil.com").body(body.clone());
    assert_eq!(foreign.send().await.unwrap().status(), StatusCode::FORBIDDEN);
    let null_origin = server.custom(put.clone(), "/api/settings", "application/json", "null").body(body.clone());
    assert_eq!(null_origin.send().await.unwrap().status(), StatusCode::FORBIDDEN);

    let local = format!("http://127.0.0.1:{}", server.addr.port());
    let form = server.custom(put.clone(), "/api/settings", "text/plain", &local).body(body.clone());
    assert_eq!(form.send().await.unwrap().status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // A local process sending the CSRF header but not this instance's token.
    let no_token = server.without_token(put.clone(), "/api/settings", "application/json", &local).body(body.clone());
    let response = no_token.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.text().await.unwrap().contains("reload the page"));
    let wrong =
        server.without_token(put, "/api/settings", "application/json", &local).header(TOKEN_HEADER, "f".repeat(64));
    assert_eq!(wrong.body(body.clone()).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    for (method, path) in [
        (reqwest::Method::POST, "/api/settings/rollback/1"),
        (reqwest::Method::DELETE, "/api/secrets/jira_api_token"),
        (reqwest::Method::PUT, "/api/secrets/jira_api_token"),
    ] {
        let request = server.without_token(method, path, "application/json", &local).body("{\"value\":\"x\"}");
        assert_eq!(request.send().await.unwrap().status(), StatusCode::UNAUTHORIZED, "{path}");
    }

    let rollback = server.client.post(server.url("/api/settings/rollback/1")).send().await.unwrap();
    assert_eq!(rollback.status(), StatusCode::FORBIDDEN);
    let delete = server.client.delete(server.url("/api/secrets/jira_api_token")).send().await.unwrap();
    assert_eq!(delete.status(), StatusCode::FORBIDDEN);
    assert!(server.credentials.get(CredentialName::JiraApiToken).unwrap().is_some());

    let huge = "x".repeat(MAX_BODY_BYTES + 1);
    let too_big = server.page(reqwest::Method::PUT, "/api/settings").body(huge).send().await.unwrap();
    assert_eq!(too_big.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let ok = server.page(reqwest::Method::PUT, "/api/settings").body(body).send().await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn host_check_still_applies_to_settings_routes() {
    let server = TestServer::start(&[]).await;
    for path in ["/settings", "/api/settings", "/api/settings/history"] {
        let response = server.client.get(server.url(path)).header(HOST, "evil.com").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let response = server.page(reqwest::Method::PUT, "/api/secrets/figma_token").header(HOST, "evil.com");
    assert_eq!(response.json(&json!({ "value": "x" })).send().await.unwrap().status(), StatusCode::FORBIDDEN);
    assert!(server.credentials.get(CredentialName::FigmaToken).unwrap().is_none());
}

#[tokio::test]
async fn secret_endpoints_never_echo_values() {
    let server = TestServer::start(&[("ANTHROPIC_API_KEY", "sk-from-env")]).await;
    let response = server
        .page(reqwest::Method::PUT, "/api/secrets/figma_token")
        .json(&json!({ "value": SECRET_VALUE }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(!text.contains(SECRET_VALUE), "{text}");
    assert_eq!(parse(&text)["secret"]["source"], "store");
    assert_eq!(server.credentials.get(CredentialName::FigmaToken).unwrap().unwrap().expose(), SECRET_VALUE);

    let bad = server.page(reqwest::Method::PUT, "/api/secrets/figma_token").json(&json!({ "value": 12345678 }));
    let response = bad.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!response.text().await.unwrap().contains("12345678"));
    let spaced =
        server.page(reqwest::Method::PUT, "/api/secrets/figma_token").json(&json!({ "value": "has space-secret" }));
    let response = spaced.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!response.text().await.unwrap().contains("space-secret"));

    let env_owned =
        server.page(reqwest::Method::PUT, "/api/secrets/anthropic_api_key").json(&json!({ "value": "sk-x" }));
    let response = env_owned.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let text = response.text().await.unwrap();
    assert!(text.contains("ANTHROPIC_API_KEY") && !text.contains("sk-from-env"), "{text}");

    let deleted = server.page(reqwest::Method::DELETE, "/api/secrets/figma_token").send().await.unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(deleted.json::<Value>().await.unwrap()["secret"]["configured"], false);
    let unknown = server.page(reqwest::Method::DELETE, "/api/secrets/nope").send().await.unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rollback_creates_a_new_version() {
    let server = TestServer::start(&[]).await;
    let mut config = server.settings().await["config"].clone();
    config["agents"]["enabled"] = json!(false);
    assert_eq!(server.put_config(json!({ "config": config })).await.0, StatusCode::OK);
    let response =
        server.page(reqwest::Method::POST, "/api/settings/rollback/1").json(&json!({})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["applied"]["version"], 3);
    assert_eq!(body["settings"]["config"]["agents"]["enabled"], true);
    assert_eq!(server.store.recent(10).unwrap()[0].changed_by, "rollback:v1");
    let missing =
        server.page(reqwest::Method::POST, "/api/settings/rollback/99").json(&json!({})).send().await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let bad = server.page(reqwest::Method::POST, "/api/settings/rollback/x").json(&json!({})).send().await.unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn serves_the_page_and_dashboard_links_to_it() {
    let server = TestServer::start(&[]).await;
    let response = server.client.get(server.url("/settings")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()[CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    let html = response.text().await.unwrap();
    assert!(html.contains("Evidence required for a failure") && html.contains("href=\"/\""));
    assert!(html.contains(&format!(r#"<meta name="qgate-token" content="{}">"#, server.token.expose())));
    assert!(html.contains(r#""X-Qgate-Token": TOKEN"#) && !html.contains("__QGATE_TOKEN__"));
    let dashboard = server.client.get(server.url("/")).send().await.unwrap().text().await.unwrap();
    assert!(dashboard.contains("href=\"/settings\""));
    assert_eq!(server.client.get(server.url("/api/nope")).send().await.unwrap().status(), StatusCode::NOT_FOUND);
}

#[test]
fn settings_html_has_no_external_resources_or_html_sinks() {
    let html = include_str!("assets/settings.html");
    for forbidden in
        ["://", "@import", "<link", "src=", "innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval("]
    {
        assert!(!html.contains(forbidden), "settings page must not contain {forbidden}");
    }
}

#[test]
fn settings_html_has_the_review_files_card() {
    let html = include_str!("assets/settings.html");
    for needle in [
        "title: \"Review files\"",
        "\"output.enabled\"",
        "\"output.path_template\"",
        "{ preview: \"output.path_template\" }",
        "\"output.formats\"",
        "\"output.include_passing\"",
        "\"output.keep_last\"",
        "\"output.rules_enabled\"",
        "\"output.rules_file\"",
        "\"output.rules_max\"",
        "value: \"html\", label: \"HTML (.html)\"",
        "f(\"output.dir\", \"Folder (in the container)\", \"readonly\"",
        "Mounted from REVIEWS_PATH on your machine",
    ] {
        assert!(html.contains(needle), "settings page must contain {needle}");
    }
}

#[tokio::test]
async fn review_file_settings_are_saved_and_validated() {
    let server = TestServer::start(&[("QGATE_OUTPUT_DIR", "/data/reviews")]).await;
    let settings = server.settings().await;
    assert_eq!(settings["config"]["output"]["formats"], json!(["markdown"]));
    assert_eq!(settings["config"]["output"]["dir"], "/data/reviews");
    assert!(
        settings["locked"]
            .as_array()
            .unwrap()
            .contains(&json!({ "field": "output.dir", "env_var": "QGATE_OUTPUT_DIR" }))
    );
    assert!(settings["restart_required_fields"].as_array().unwrap().contains(&json!("output.dir")));

    let mut config = settings["config"].clone();
    config["output"]["path_template"] = json!("{repo}/{date}-{sha7}.md");
    config["output"]["formats"] = json!(["markdown", "json", "html"]);
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body = parse(&text);
    assert_eq!(body["applied"]["restart_required"], json!([]), "template and formats apply live");
    assert_eq!(server.store.load_raw().unwrap().unwrap().output.dir, "/reviews", "env value is not stored");

    let mut config = server.settings().await["config"].clone();
    config["output"]["path_template"] = json!("../{sha7}.md");
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(parse(&text)["field"], "output.path_template");
}

#[tokio::test]
async fn endpoints_commands_and_webhook_are_read_only_over_http() {
    let server = TestServer::start(&[("QGATE_LLM_BASE_URL", "http://127.0.0.1:4000")]).await;
    let settings = server.settings().await;
    let admin_only = settings["admin_only"].as_array().unwrap();
    for (field, env_var) in [
        ("llm.base_url", "QGATE_LLM_BASE_URL"),
        ("jira.mcp.command", "QGATE_JIRA_MCP_COMMAND"),
        ("notify.webhook_url", "QGATE_WEBHOOK_URL"),
        ("watch.run_repo_checks", "QGATE_RUN_REPO_CHECKS"),
    ] {
        assert!(admin_only.contains(&json!({ "field": field, "env_var": env_var })), "{field}");
    }
    let base = settings["config"].clone();
    let changes: [(&str, &str, Value); 6] = [
        ("llm", "base_url", json!("https://attacker.example")),
        ("jira", "base_url", json!("https://attacker.example")),
        ("figma", "api_base_url", json!("https://attacker.example")),
        ("figma", "mcp_url", json!("https://attacker.example/mcp")),
        ("jira", "mcp", json!({ "command": ["sh", "-c", "id"], "tool": "jira_get_issue", "env_passthrough": [] })),
        (
            "jira",
            "mcp",
            json!({
                "command": ["uvx", "mcp-atlassian==0.23.1"],
                "tool": "jira_get_issue",
                "env_passthrough": ["ANTHROPIC_API_KEY"],
            }),
        ),
    ];
    for (section, key, value) in changes {
        let mut config = base.clone();
        config[section][key] = value;
        let (status, text) = server.put_config(json!({ "config": config })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{section}.{key}: {text}");
        assert!(parse(&text)["error"].as_str().unwrap().contains("admin-only"), "{text}");
    }
    assert_eq!(server.store.recent(10).unwrap().len(), 1, "nothing was saved");

    // Tightening stays allowed: repository checks off, webhook cleared.
    let mut config = base.clone();
    config["watch"]["run_repo_checks"] = json!(false);
    let (status, text) = server.put_config(json!({ "config": config, "webhook": { "action": "clear" } })).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let saved = server.store.load_raw().unwrap().unwrap();
    assert!(!saved.watch.run_repo_checks && saved.notify.webhook_url.is_empty());
    assert_eq!(saved.llm.base_url, "https://api.anthropic.com", "the env value is not persisted");

    // ...but turning them back on is not.
    let mut config = server.settings().await["config"].clone();
    config["watch"]["run_repo_checks"] = json!(true);
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(parse(&text)["field"], "watch.run_repo_checks");
    assert!(parse(&text)["error"].as_str().unwrap().contains("QGATE_RUN_REPO_CHECKS"));
}

#[test]
fn settings_html_renders_admin_only_fields_read_only() {
    let html = include_str!("assets/settings.html");
    for needle in ["state.view.admin_only", "Read-only here for security", "isReadOnly(def.path, value)"] {
        assert!(html.contains(needle), "settings page must contain {needle}");
    }
}

#[test]
fn settings_html_has_the_quality_and_regression_toggles() {
    let html = include_str!("assets/settings.html");
    for needle in ["f(\"quality.enabled\"", "f(\"regression.enabled\"", "\"bool\""] {
        assert!(html.contains(needle), "settings page must contain {needle}");
    }
}

#[tokio::test]
async fn quality_and_regression_toggles_default_on_save_over_http_and_lock_to_env() {
    let server = TestServer::start(&[]).await;
    let mut config = server.settings().await["config"].clone();
    assert_eq!((&config["quality"]["enabled"], &config["regression"]["enabled"]), (&json!(true), &json!(true)));
    config["quality"]["enabled"] = json!(false);
    config["regression"]["enabled"] = json!(false);
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let saved = server.store.load_raw().unwrap().unwrap();
    assert!(!saved.quality.enabled && !saved.regression.enabled);
    let mut config = server.settings().await["config"].clone();
    config["quality"]["enabled"] = json!(true);
    let (status, text) = server.put_config(json!({ "config": config })).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(server.store.load_raw().unwrap().unwrap().quality.enabled);

    let locked = TestServer::start(&[("QGATE_REGRESSION_ENABLED", "false")]).await;
    let settings = locked.settings().await;
    assert_eq!(settings["config"]["regression"]["enabled"], json!(false));
    assert!(settings["locked"].as_array().unwrap().iter().any(|l| l["field"] == "regression.enabled"), "{settings}");
}
