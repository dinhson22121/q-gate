mod page;

use axum::extract::{Query, Request, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::access::domain::{DashboardSecret, SessionKey};
use page::LoginPage;

pub const SESSION_COOKIE: &str = "qgate_session";
pub const LOGIN_PATH: &str = "/login";
pub const HEALTH_PATH: &str = "/healthz";
const PUBLIC_PATHS: &[&str] = &["/api/jira/oauth/callback"];
pub const SESSION_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;
const API_PREFIX: &str = "/api/";
const LOGIN_REQUIRED: &str =
    "login required: open the login link printed in `docker compose logs qgate` (or run `qgate login-url`)";

#[derive(Clone)]
pub struct AccessState {
    secret: DashboardSecret,
    session: SessionKey,
}

impl AccessState {
    pub fn new(secret: DashboardSecret) -> Self {
        let session = secret.session_key();
        Self { secret, session }
    }
}

pub fn protect(app: Router, state: AccessState) -> Router {
    let open = Router::new()
        .route(LOGIN_PATH, get(login_link).post(login_form))
        .route(HEALTH_PATH, get(healthz))
        .with_state(state.clone());
    app.layer(middleware::from_fn_with_state(state, require_session)).merge(open)
}

#[derive(Debug, Deserialize)]
struct LoginParams {
    token: Option<String>,
}

async fn require_session(State(state): State<AccessState>, request: Request, next: Next) -> Response {
    if has_session(request.headers(), &state.session) {
        return next.run(request).await;
    }
    let path = request.uri().path();
    if PUBLIC_PATHS.contains(&path) {
        return next.run(request).await;
    }
    if path.starts_with(API_PREFIX) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": LOGIN_REQUIRED }))).into_response();
    }
    Redirect::to(LOGIN_PATH).into_response()
}

async fn login_link(
    State(state): State<AccessState>,
    headers: HeaderMap,
    Query(params): Query<LoginParams>,
) -> Response {
    match params.token {
        Some(token) => login(&state, &token),
        None if has_session(&headers, &state.session) => Redirect::to("/").into_response(),
        None => LoginPage::Required.into_response(),
    }
}

async fn login_form(State(state): State<AccessState>, Form(params): Form<LoginParams>) -> Response {
    login(&state, params.token.as_deref().unwrap_or_default())
}

async fn healthz() -> &'static str {
    "ok"
}

fn login(state: &AccessState, token: &str) -> Response {
    if !state.secret.verify(token.trim()) {
        return LoginPage::Invalid.into_response();
    }
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(SET_COOKIE, session_cookie(&state.session));
    response
}

fn session_cookie(session: &SessionKey) -> HeaderValue {
    let cookie = format!(
        "{SESSION_COOKIE}={}; Max-Age={SESSION_MAX_AGE_SECS}; Path=/; HttpOnly; SameSite=Strict",
        session.expose()
    );
    HeaderValue::from_str(&cookie).expect("hex session key is a valid header value")
}

fn has_session(headers: &HeaderMap, session: &SessionKey) -> bool {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == SESSION_COOKIE && session.verify(value))
}

#[cfg(test)]
mod tests;
