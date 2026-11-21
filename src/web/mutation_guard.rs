//! Checks for the mutating routes of the settings, rulebook and policy APIs (GET/HEAD pass through):
//! - a custom `X-Qgate-Request` header and `Content-Type: application/json`: both force a CORS preflight,
//!   which this server never answers, so a cross-site page cannot send them (CSRF);
//! - an `Origin` allow-list as defence in depth;
//! - the per-instance [`ApiToken`] (401 when missing or wrong), against processes that are not our pages.

use axum::http::header::{CONTENT_TYPE, ORIGIN};
use axum::http::{HeaderMap, Method, StatusCode};

use super::api_token::{ApiToken, TOKEN_HEADER};

pub const REQUEST_HEADER: &str = "x-qgate-request";
const JSON_MIME: &str = "application/json";

/// Why a request was refused; each API renders it in its own error body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejection {
    pub status: StatusCode,
    pub message: &'static str,
}

impl Rejection {
    const fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }
}

#[derive(Debug, Clone)]
pub struct MutationGuard {
    /// Expected `X-Qgate-Request` value, e.g. `settings`.
    request_value: &'static str,
    allowed_hosts: Vec<String>,
    token: ApiToken,
}

impl MutationGuard {
    /// `allowed_hosts`: the dashboard's `dashboard.allowed_hosts`; an `Origin` header must name one of them.
    pub fn new(request_value: &'static str, allowed_hosts: &[String], token: ApiToken) -> Self {
        let allowed_hosts = allowed_hosts.iter().map(|h| normalize_host(h)).filter(|h| !h.is_empty()).collect();
        Self { request_value, allowed_hosts, token }
    }

    pub fn token(&self) -> &ApiToken {
        &self.token
    }

    pub fn check(&self, method: &Method, headers: &HeaderMap) -> Result<(), Rejection> {
        if matches!(*method, Method::GET | Method::HEAD) {
            return Ok(());
        }
        let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        if text(REQUEST_HEADER) != Some(self.request_value) {
            return Err(Rejection::new(StatusCode::FORBIDDEN, "missing X-Qgate-Request header"));
        }
        if let Some(origin) = headers.get(ORIGIN) {
            let allowed =
                origin.to_str().ok().and_then(origin_host).is_some_and(|host| self.allowed_hosts.contains(&host));
            if !allowed {
                return Err(Rejection::new(StatusCode::FORBIDDEN, "origin not allowed"));
            }
        }
        if !self.token.verify(text(TOKEN_HEADER)) {
            return Err(Rejection::new(
                StatusCode::UNAUTHORIZED,
                "missing or invalid X-Qgate-Token (qgate restarted?): reload the page",
            ));
        }
        if !text(CONTENT_TYPE.as_str()).is_some_and(is_json) {
            return Err(Rejection::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Content-Type must be application/json"));
        }
        Ok(())
    }
}

/// Lowercased host without port; IPv6 brackets removed (`[::1]:8080` → `::1`).
pub fn normalize_host(raw: &str) -> String {
    let raw = raw.trim();
    let host = match raw.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None if raw.matches(':').count() > 1 => raw,
        None => raw.split(':').next().unwrap_or_default(),
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Host of an `Origin` header (`http://localhost:8080` → `localhost`); `None` for `null` or garbage.
fn origin_host(origin: &str) -> Option<String> {
    let (_, rest) = origin.split_once("://")?;
    let host = normalize_host(rest.split('/').next().unwrap_or_default());
    (!host.is_empty()).then_some(host)
}

fn is_json(content_type: &str) -> bool {
    content_type.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case(JSON_MIME))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn guard() -> MutationGuard {
        MutationGuard::new("settings", &["LocalHost".into(), " ".into()], ApiToken::generate().unwrap())
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn page_headers(guard: &MutationGuard) -> Vec<(&'static str, String)> {
        vec![
            (REQUEST_HEADER, "settings".into()),
            ("origin", "http://localhost:8080".into()),
            (TOKEN_HEADER, guard.token().expose().into()),
            ("content-type", "application/json; charset=utf-8".into()),
        ]
    }

    fn check_without(guard: &MutationGuard, drop: &str, replace: Option<&str>) -> Result<(), Rejection> {
        let pairs: Vec<(&'static str, String)> = page_headers(guard)
            .into_iter()
            .filter_map(|(k, v)| match (k == drop, replace) {
                (false, _) => Some((k, v)),
                (true, Some(other)) => Some((k, other.to_string())),
                (true, None) => None,
            })
            .collect();
        let borrowed: Vec<(&'static str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        guard.check(&Method::PUT, &headers(&borrowed))
    }

    #[test]
    fn page_requests_pass_and_reads_are_never_checked() {
        let guard = guard();
        assert_eq!(check_without(&guard, "none", None), Ok(()));
        assert_eq!(check_without(&guard, "origin", None), Ok(()), "Origin is optional");
        assert_eq!(guard.check(&Method::GET, &HeaderMap::new()), Ok(()));
        assert_eq!(guard.check(&Method::HEAD, &HeaderMap::new()), Ok(()));
    }

    #[test]
    fn each_missing_piece_has_its_own_status() {
        let guard = guard();
        let status = |drop, replace| check_without(&guard, drop, replace).unwrap_err().status;
        assert_eq!(status(REQUEST_HEADER, None), StatusCode::FORBIDDEN);
        assert_eq!(status(REQUEST_HEADER, Some("rulebook")), StatusCode::FORBIDDEN);
        assert_eq!(status("origin", Some("http://evil.example")), StatusCode::FORBIDDEN);
        assert_eq!(status("origin", Some("null")), StatusCode::FORBIDDEN);
        assert_eq!(status(TOKEN_HEADER, None), StatusCode::UNAUTHORIZED);
        assert_eq!(status(TOKEN_HEADER, Some("00")), StatusCode::UNAUTHORIZED);
        assert_eq!(status("content-type", Some("text/plain")), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(check_without(&guard, TOKEN_HEADER, None).unwrap_err().message.contains("reload the page"));
    }

    #[test]
    fn parses_origins_and_content_types() {
        assert_eq!(origin_host("http://LocalHost:8080").as_deref(), Some("localhost"));
        assert_eq!(origin_host("http://[::1]:8080/x").as_deref(), Some("::1"));
        assert_eq!(origin_host("null"), None);
        assert_eq!(origin_host("http://"), None);
        assert!(is_json("application/json; charset=utf-8"));
        assert!(!is_json("text/plain"));
        assert_eq!(normalize_host("Example.com."), "example.com");
        assert_eq!(normalize_host("::1"), "::1");
    }
}
