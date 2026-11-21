//! Login and the session check in front of every dashboard route.
//!
//! - `GET /login?token=<secret>` (the link printed at startup) or `POST /login` (form field `token`):
//!   sets the `qgate_session` cookie and redirects to `/`, which also drops the token from the address bar.
//! - Every other route (pages, `/api/*`, unknown paths) needs that cookie: pages redirect to `/login`, the
//!   JSON API answers 401.
//! - `GET /healthz` is open and returns only `ok` (container health check).

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
/// The session lasts 30 days; the browser then needs the login link again.
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

/// `app` with the session check on all its routes and fallback, plus the open `/login` and `/healthz`.
/// `app` must not define those two paths.
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
    if request.uri().path().starts_with(API_PREFIX) {
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
        // Already logged in (e.g. a bookmarked /login): nothing to do.
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

/// HttpOnly (no script can read it), SameSite=Strict (never sent on cross-site requests), whole site.
/// No `Secure`: the dashboard is plain http on localhost.
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
