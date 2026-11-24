use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::tickets::application::{Authorization, JiraOauthGateway, OauthError, PendingAuthorization};
use crate::tickets::domain::{TokenBundle, base64url};

pub const ATLASSIAN_MCP_URL: &str = "https://mcp.atlassian.com/v2/mcp";
pub const OAUTH_SCOPE: &str = "offline_access read:jira:agent-interface";
const CLIENT_NAME: &str = "qgate";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_EXPIRES_IN_SECONDS: i64 = 3600;
const PKCE_VERIFIER_BYTES: usize = 32;
const STATE_BYTES: usize = 16;
const RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";
const AUTH_SERVER_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
}

#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Result<Self, OauthError> {
        let verifier = random_token(PKCE_VERIFIER_BYTES)?;
        let challenge = base64url::encode(Sha256::digest(verifier.as_bytes()).as_slice());
        Ok(Self { verifier, challenge })
    }
}

fn random_token(len: usize) -> Result<String, OauthError> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|_| OauthError::Token("random generator unavailable".into()))?;
    Ok(base64url::encode(&bytes))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

pub struct JiraOauthClient {
    http: reqwest::Client,
    resource_url: String,
}

impl JiraOauthClient {
    pub fn atlassian() -> Self {
        Self::new(ATLASSIAN_MCP_URL)
    }

    pub fn new(resource_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap_or_default();
        Self { http, resource_url: resource_url.into() }
    }
}

impl JiraOauthGateway for JiraOauthClient {
    fn begin<'a>(
        &'a self,
        site_hostname: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Authorization, OauthError>> {
        Box::pin(async move {
            let endpoints = discover(&self.http, &self.resource_url).await?;
            let client_id = register(&self.http, &endpoints.registration_endpoint, redirect_uri).await?;
            let pkce = Pkce::generate()?;
            let state = random_token(STATE_BYTES)?;
            let authorize_url =
                build_authorize_url(&endpoints.authorization_endpoint, &client_id, redirect_uri, &state, &pkce)?;
            let pending = PendingAuthorization {
                state,
                code_verifier: pkce.verifier,
                client_id,
                token_endpoint: endpoints.token_endpoint,
                redirect_uri: redirect_uri.to_string(),
                site_hostname: site_hostname.to_string(),
            };
            Ok(Authorization { authorize_url, pending })
        })
    }

    fn exchange<'a>(
        &'a self,
        pending: &'a PendingAuthorization,
        code: &'a str,
    ) -> BoxFuture<'a, Result<TokenBundle, OauthError>> {
        Box::pin(async move { exchange_code(&self.http, pending, code, Utc::now()).await })
    }

    fn refresh<'a>(&'a self, bundle: &'a TokenBundle) -> BoxFuture<'a, Result<TokenBundle, OauthError>> {
        Box::pin(async move { refresh_tokens(&self.http, bundle, Utc::now()).await })
    }
}

fn discovery_error(message: impl Into<String>) -> OauthError {
    OauthError::Discovery(message.into())
}

fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

async fn fetch_json(http: &reqwest::Client, url: &str) -> Result<Value, OauthError> {
    let response = http.get(url).send().await.map_err(|e| discovery_error(e.without_url().to_string()))?;
    if !response.status().is_success() {
        return Err(discovery_error(format!("HTTP {} from {url}", response.status())));
    }
    response.json().await.map_err(|_| discovery_error(format!("invalid metadata from {url}")))
}

fn resource_metadata_url(www_authenticate: &str) -> Option<String> {
    let rest = www_authenticate.split_once("resource_metadata=\"")?.1;
    rest.split_once('"').map(|(url, _)| url.to_string())
}

async fn protected_resource_metadata_url(http: &reqwest::Client, resource: &Url) -> String {
    let fallback = format!("{}{RESOURCE_METADATA_PATH}", origin_of(resource));
    let Ok(response) = http.get(resource.as_str()).send().await else {
        return fallback;
    };
    response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .and_then(resource_metadata_url)
        .unwrap_or(fallback)
}

async fn authorization_server(http: &reqwest::Client, resource: &Url) -> Url {
    let metadata_url = protected_resource_metadata_url(http, resource).await;
    let advertised = fetch_json(http, &metadata_url)
        .await
        .ok()
        .and_then(|metadata| metadata["authorization_servers"][0].as_str().map(str::to_string))
        .and_then(|server| Url::parse(&server).ok());
    advertised.unwrap_or_else(|| Url::parse(&origin_of(resource)).unwrap_or_else(|_| resource.clone()))
}

fn auth_server_metadata_url(server: &Url) -> String {
    let path = server.path().trim_end_matches('/');
    format!("{}{AUTH_SERVER_METADATA_PATH}{path}", origin_of(server))
}

fn endpoint(metadata: &Value, field: &str, scheme: &str) -> Result<String, OauthError> {
    let value = metadata[field].as_str().ok_or_else(|| discovery_error(format!("metadata lacks {field}")))?;
    let parsed = Url::parse(value).map_err(|_| discovery_error(format!("{field} is not a URL")))?;
    if parsed.scheme() != scheme {
        return Err(discovery_error(format!("{field} must use {scheme}")));
    }
    Ok(value.to_string())
}

pub async fn discover(http: &reqwest::Client, resource_url: &str) -> Result<Endpoints, OauthError> {
    let resource = Url::parse(resource_url).map_err(|_| discovery_error("resource URL is invalid"))?;
    let server = authorization_server(http, &resource).await;
    let metadata = fetch_json(http, &auth_server_metadata_url(&server)).await?;
    let scheme = resource.scheme();
    Ok(Endpoints {
        authorization_endpoint: endpoint(&metadata, "authorization_endpoint", scheme)?,
        token_endpoint: endpoint(&metadata, "token_endpoint", scheme)?,
        registration_endpoint: endpoint(&metadata, "registration_endpoint", scheme)?,
    })
}

pub async fn register(
    http: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> Result<String, OauthError> {
    let body = json!({
        "client_name": CLIENT_NAME,
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "scope": OAUTH_SCOPE,
    });
    let registration_error = |message: String| OauthError::Registration(message);
    let response = http
        .post(registration_endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| registration_error(e.without_url().to_string()))?;
    if !response.status().is_success() {
        return Err(registration_error(format!("HTTP {}", response.status())));
    }
    let registered: Value = response.json().await.map_err(|_| registration_error("invalid response".into()))?;
    registered["client_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| registration_error("response lacks client_id".into()))
}

pub fn build_authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    pkce: &Pkce,
) -> Result<String, OauthError> {
    let mut url =
        Url::parse(authorization_endpoint).map_err(|_| discovery_error("authorization endpoint is not a URL"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", OAUTH_SCOPE)
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.into())
}

async fn token_request(
    http: &reqwest::Client,
    token_endpoint: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, OauthError> {
    let token_error = |message: String| OauthError::Token(message);
    let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(form).finish();
    let response = http
        .post(token_endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| token_error(e.without_url().to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let reason = response.json::<Value>().await.ok().and_then(|v| v["error"].as_str().map(str::to_string));
        return Err(token_error(format!("HTTP {status}{}", reason.map(|r| format!(" ({r})")).unwrap_or_default())));
    }
    response.json().await.map_err(|_| token_error("invalid token response".into()))
}

fn expiry(now: DateTime<Utc>, expires_in: Option<i64>) -> DateTime<Utc> {
    now + chrono::Duration::seconds(expires_in.unwrap_or(DEFAULT_EXPIRES_IN_SECONDS))
}

pub async fn exchange_code(
    http: &reqwest::Client,
    pending: &PendingAuthorization,
    code: &str,
    now: DateTime<Utc>,
) -> Result<TokenBundle, OauthError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", pending.redirect_uri.as_str()),
        ("client_id", pending.client_id.as_str()),
        ("code_verifier", pending.code_verifier.as_str()),
    ];
    let tokens = token_request(http, &pending.token_endpoint, &form).await?;
    Ok(TokenBundle {
        client_id: pending.client_id.clone(),
        token_endpoint: pending.token_endpoint.clone(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: expiry(now, tokens.expires_in),
        site_hostname: pending.site_hostname.clone(),
    })
}

pub async fn refresh_tokens(
    http: &reqwest::Client,
    bundle: &TokenBundle,
    now: DateTime<Utc>,
) -> Result<TokenBundle, OauthError> {
    let refresh_token = bundle.refresh_token.as_deref().ok_or_else(|| OauthError::Token("no refresh token".into()))?;
    let form =
        [("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", bundle.client_id.as_str())];
    let tokens = token_request(http, &bundle.token_endpoint, &form).await?;
    Ok(TokenBundle {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token.or_else(|| bundle.refresh_token.clone()),
        expires_at: expiry(now, tokens.expires_in),
        ..bundle.clone()
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{body_json, body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const REDIRECT: &str = "http://localhost:8080/api/jira/oauth/callback";

    fn http() -> reqwest::Client {
        reqwest::Client::new()
    }

    async fn mount_discovery(server: &MockServer) {
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/v2/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "WWW-Authenticate",
                format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\""),
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "authorization_servers": [base] })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn discovers_endpoints_through_protected_resource_metadata() {
        let server = MockServer::start().await;
        mount_discovery(&server).await;
        let endpoints = discover(&http(), &format!("{}/v2/mcp", server.uri())).await.unwrap();
        assert_eq!(endpoints.authorization_endpoint, format!("{}/authorize", server.uri()));
        assert_eq!(endpoints.token_endpoint, format!("{}/token", server.uri()));
        assert_eq!(endpoints.registration_endpoint, format!("{}/register", server.uri()));
    }

    #[tokio::test]
    async fn falls_back_to_the_resource_origin_when_resource_metadata_is_missing() {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "authorization_endpoint": format!("{base}/a"),
                "token_endpoint": format!("{base}/t"),
                "registration_endpoint": format!("{base}/r"),
            })))
            .mount(&server)
            .await;
        let endpoints = discover(&http(), &format!("{base}/v2/mcp")).await.unwrap();
        assert_eq!(endpoints.token_endpoint, format!("{base}/t"));
    }

    #[tokio::test]
    async fn discovery_rejects_endpoints_with_a_different_scheme() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "authorization_endpoint": "https://evil.example/a",
                "token_endpoint": "https://evil.example/t",
                "registration_endpoint": "https://evil.example/r",
            })))
            .mount(&server)
            .await;
        let error = discover(&http(), &format!("{}/v2/mcp", server.uri())).await.unwrap_err();
        assert!(matches!(error, OauthError::Discovery(_)), "{error}");
    }

    #[tokio::test]
    async fn registers_a_public_client_with_the_redirect_uri() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .and(body_json(json!({
                "client_name": "qgate",
                "redirect_uris": [REDIRECT],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
                "scope": "offline_access read:jira:agent-interface",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "client_id": "dcr-client" })))
            .expect(1)
            .mount(&server)
            .await;
        let client_id = register(&http(), &format!("{}/register", server.uri()), REDIRECT).await.unwrap();
        assert_eq!(client_id, "dcr-client");
    }

    #[tokio::test]
    async fn registration_failure_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(400)).mount(&server).await;
        let error = register(&http(), &format!("{}/register", server.uri()), REDIRECT).await.unwrap_err();
        assert!(matches!(error, OauthError::Registration(_)));
    }

    #[test]
    fn pkce_challenge_is_the_s256_of_a_valid_verifier() {
        let pkce = Pkce::generate().unwrap();
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert!(pkce.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(pkce.challenge, base64url::encode(Sha256::digest(pkce.verifier.as_bytes()).as_slice()));
        assert_ne!(pkce.verifier, Pkce::generate().unwrap().verifier);
    }

    #[test]
    fn pkce_matches_the_rfc7636_example() {
        let digest = Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(base64url::encode(digest.as_slice()), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn authorize_url_carries_all_oauth_parameters() {
        let pkce = Pkce::generate().unwrap();
        let url = build_authorize_url("https://auth.example/authorize", "cid", REDIRECT, "st", &pkce).unwrap();
        let parsed = Url::parse(&url).unwrap();
        let query: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], "cid");
        assert_eq!(query["redirect_uri"], REDIRECT);
        assert_eq!(query["scope"], "offline_access read:jira:agent-interface");
        assert_eq!(query["state"], "st");
        assert_eq!(query["code_challenge"], pkce.challenge);
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(!url.contains(&pkce.verifier));
    }

    fn pending(server: &MockServer) -> PendingAuthorization {
        PendingAuthorization {
            state: "st".into(),
            code_verifier: "the-verifier".into(),
            client_id: "dcr-client".into(),
            token_endpoint: format!("{}/token", server.uri()),
            redirect_uri: REDIRECT.into(),
            site_hostname: "acme.atlassian.net".into(),
        }
    }

    #[tokio::test]
    async fn exchanges_the_code_with_the_pkce_verifier() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=auth-code"))
            .and(body_string_contains("code_verifier=the-verifier"))
            .and(body_string_contains("client_id=dcr-client"))
            .and(body_string_contains("redirect_uri="))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "access_token": "acc", "refresh_token": "ref", "expires_in": 600, "token_type": "Bearer" }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let now = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let bundle = exchange_code(&http(), &pending(&server), "auth-code", now).await.unwrap();
        assert_eq!(bundle.access_token, "acc");
        assert_eq!(bundle.refresh_token.as_deref(), Some("ref"));
        assert_eq!(bundle.expires_at, now + chrono::Duration::seconds(600));
        assert_eq!(bundle.site_hostname, "acme.atlassian.net");
    }

    #[tokio::test]
    async fn token_error_does_not_leak_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant", "x": "secret" })))
            .mount(&server)
            .await;
        let now = DateTime::from_timestamp(0, 0).unwrap();
        let error = exchange_code(&http(), &pending(&server), "c", now).await.unwrap_err().to_string();
        assert!(error.contains("invalid_grant") && !error.contains("secret"), "{error}");
    }

    #[tokio::test]
    async fn refresh_keeps_the_old_refresh_token_when_none_is_returned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-ref"))
            .and(body_string_contains("client_id=dcr-client"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "new-acc" })))
            .expect(1)
            .mount(&server)
            .await;
        let now = DateTime::from_timestamp(1_000, 0).unwrap();
        let old = TokenBundle {
            client_id: "dcr-client".into(),
            token_endpoint: format!("{}/token", server.uri()),
            access_token: "old-acc".into(),
            refresh_token: Some("old-ref".into()),
            expires_at: now,
            site_hostname: "acme.atlassian.net".into(),
        };
        let fresh = refresh_tokens(&http(), &old, now).await.unwrap();
        assert_eq!(fresh.access_token, "new-acc");
        assert_eq!(fresh.refresh_token.as_deref(), Some("old-ref"));
        assert_eq!(fresh.expires_at, now + chrono::Duration::seconds(DEFAULT_EXPIRES_IN_SECONDS));
    }

    #[tokio::test]
    async fn refresh_without_a_refresh_token_fails() {
        let now = DateTime::from_timestamp(0, 0).unwrap();
        let bundle = TokenBundle {
            client_id: "c".into(),
            token_endpoint: "http://127.0.0.1:1/token".into(),
            access_token: "a".into(),
            refresh_token: None,
            expires_at: now,
            site_hostname: "h".into(),
        };
        assert!(matches!(refresh_tokens(&http(), &bundle, now).await, Err(OauthError::Token(_))));
    }

    #[tokio::test]
    async fn begin_runs_discovery_registration_and_builds_the_authorize_url() {
        let server = MockServer::start().await;
        mount_discovery(&server).await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "client_id": "dcr-client" })))
            .mount(&server)
            .await;
        let client = JiraOauthClient::new(format!("{}/v2/mcp", server.uri()));
        let authorization = client.begin("acme.atlassian.net", REDIRECT).await.unwrap();
        assert!(authorization.authorize_url.starts_with(&format!("{}/authorize?", server.uri())));
        assert!(authorization.authorize_url.contains(&format!("state={}", authorization.pending.state)));
        assert_eq!(authorization.pending.client_id, "dcr-client");
        assert_eq!(authorization.pending.token_endpoint, format!("{}/token", server.uri()));
    }
}
