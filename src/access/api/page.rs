//! The 401 login page. Static text only: it never echoes what was submitted.

use axum::http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

const LOGIN_HTML: &str = include_str!("login.html");
const MESSAGE_PLACEHOLDER: &str = "__MESSAGE__";
const RETRY_PLACEHOLDER: &str = "__RETRY__";
/// Own policy (the dashboard's forbids forms): inline style/script, the form may post to this origin only.
const LOGIN_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoginPage {
    /// No session and no token.
    Required,
    /// A token was presented and is wrong.
    Invalid,
}

impl LoginPage {
    fn message(self) -> &'static str {
        match self {
            Self::Required => "Log in with the dashboard link to continue.",
            Self::Invalid => "That token is not valid. Use the link printed by the current qgate service.",
        }
    }

    /// `Required` retries `/` once per tab: a SameSite=Strict cookie is not sent when the dashboard is
    /// opened from a link on another site, but is on this same-site navigation.
    fn html(self) -> String {
        let retry = matches!(self, Self::Required);
        LOGIN_HTML.replace(MESSAGE_PLACEHOLDER, self.message()).replace(RETRY_PLACEHOLDER, &retry.to_string())
    }
}

impl IntoResponse for LoginPage {
    fn into_response(self) -> Response {
        let mut response = (StatusCode::UNAUTHORIZED, self.html()).into_response();
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
        headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(LOGIN_CSP));
        response
    }
}
