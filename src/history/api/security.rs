use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;

use super::DashboardState;
use super::handlers::error_body;

const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub(super) fn normalize_host(raw: &str) -> String {
    let raw = raw.trim();
    let host = match raw.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None if raw.matches(':').count() > 1 => raw,
        None => raw.split(':').next().unwrap_or_default(),
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

pub(super) async fn check_host(State(state): State<DashboardState>, request: Request, next: Next) -> Response<Body> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| request.uri().authority().map(|a| a.as_str()))
        .map(normalize_host);
    match host {
        Some(host) if state.allowed_hosts.contains(&host) => next.run(request).await,
        _ => (StatusCode::FORBIDDEN, error_body("host not allowed")).into_response(),
    }
}

pub(super) async fn security_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.entry(header::CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static(CSP));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
