//! HTTP transport to `POST /v1/messages`: headers, retries, redirects and error classification.

use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Response, StatusCode};
use serde::Deserialize;

use super::config::ClaudeConfig;
use crate::llm::LlmError;

const MESSAGES_PATH: &str = "/v1/messages";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const HEADER_API_KEY: &str = "x-api-key";
const HEADER_VERSION: &str = "anthropic-version";
const HEADER_BETA: &str = "anthropic-beta";
const HEADER_SHOULD_RETRY: &str = "x-should-retry";
const HEADER_RETRY_AFTER: &str = "retry-after";
const JSON: &str = "application/json";
/// Retries after the first attempt.
const MAX_RETRIES: u32 = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const MAX_RAW_ERROR_CHARS: usize = 300;

/// Sends request bodies. Deliberately not `Debug`: its HTTP client carries the API key.
pub(super) struct Transport {
    http: reqwest::Client,
    endpoint: String,
    retry_base_delay: Duration,
}

/// Outcome of a single failed attempt.
enum Failure {
    Retry { reason: String, retry_after: Option<Duration> },
    Fatal(LlmError),
}

impl Transport {
    pub(super) fn new(api_key: &str, config: &ClaudeConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .default_headers(default_headers(api_key.trim(), config.use_fallbacks)?)
            .timeout(config.request_timeout)
            // Never follow redirects: reqwest would replay the custom `x-api-key` header to the new host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| LlmError::Rejected(format!("cannot build HTTP client: {e}")))?;
        let endpoint = format!("{}{MESSAGES_PATH}", config.base_url.trim_end_matches('/'));
        Ok(Self { http, endpoint, retry_base_delay: config.retry_base_delay })
    }

    /// Posts the JSON `body`, retrying transient failures; returns the raw success body.
    pub(super) async fn post(&self, body: &str) -> Result<String, LlmError> {
        let mut attempt = 0;
        loop {
            match self.post_once(body).await {
                Ok(text) => return Ok(text),
                Err(Failure::Fatal(error)) => return Err(error),
                Err(Failure::Retry { reason, .. }) if attempt >= MAX_RETRIES => {
                    return Err(LlmError::Unavailable(reason));
                }
                Err(Failure::Retry { retry_after, .. }) => {
                    tokio::time::sleep(retry_delay(self.retry_base_delay, attempt, retry_after)).await;
                    attempt += 1;
                }
            }
        }
    }

    async fn post_once(&self, body: &str) -> Result<String, Failure> {
        let response = self
            .http
            .post(&self.endpoint)
            .header(CONTENT_TYPE, JSON)
            .body(body.to_owned())
            .send()
            .await
            .map_err(transport_failure)?;
        let status = response.status();
        if status.is_success() {
            return response.text().await.map_err(transport_failure);
        }
        if status.is_redirection() {
            return Err(Failure::Fatal(LlmError::Rejected(format!(
                "unexpected redirect (HTTP {}) from the Anthropic API; redirects are not followed, check llm.base_url",
                status.as_u16()
            ))));
        }
        Err(status_failure(status, response).await)
    }
}

fn default_headers(api_key: &str, use_fallbacks: bool) -> Result<HeaderMap, LlmError> {
    if api_key.is_empty() {
        return Err(LlmError::Rejected("Anthropic API key is empty".into()));
    }
    let mut key = HeaderValue::from_str(api_key)
        .map_err(|_| LlmError::Rejected("Anthropic API key contains invalid characters".into()))?;
    key.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(HeaderName::from_static(HEADER_API_KEY), key);
    headers.insert(HeaderName::from_static(HEADER_VERSION), HeaderValue::from_static(API_VERSION));
    if use_fallbacks {
        headers.insert(HeaderName::from_static(HEADER_BETA), HeaderValue::from_static(FALLBACK_BETA));
    }
    Ok(headers)
}

fn transport_failure(error: reqwest::Error) -> Failure {
    let is_transient = error.is_connect() || error.is_timeout();
    let reason = error.without_url().to_string();
    if is_transient {
        Failure::Retry { reason, retry_after: None }
    } else {
        Failure::Fatal(LlmError::Unavailable(reason))
    }
}

async fn status_failure(status: StatusCode, response: Response) -> Failure {
    let should_retry = header_str(&response, HEADER_SHOULD_RETRY).map(|v| v.eq_ignore_ascii_case("true"));
    let retry_after = header_str(&response, HEADER_RETRY_AFTER).and_then(parse_retry_after);
    let body = response.text().await.unwrap_or_default();
    classify_status(status, should_retry, retry_after, describe_error(status.as_u16(), &body))
}

fn header_str(response: &Response, name: &str) -> Option<String> {
    response.headers().get(name)?.to_str().ok().map(|v| v.trim().to_string())
}

/// Decides whether an HTTP error is retried. `should_retry` is the `x-should-retry` header, if any.
fn classify_status(
    status: StatusCode,
    should_retry: Option<bool>,
    retry_after: Option<Duration>,
    reason: String,
) -> Failure {
    let retryable = should_retry.unwrap_or_else(|| is_retryable_status(status));
    if retryable {
        Failure::Retry { reason, retry_after }
    } else if status.is_client_error() && !is_retryable_status(status) {
        Failure::Fatal(LlmError::Rejected(reason))
    } else {
        Failure::Fatal(LlmError::Unavailable(reason))
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error()
}

fn parse_retry_after(value: String) -> Option<Duration> {
    value.parse::<f64>().ok().and_then(|secs| Duration::try_from_secs_f64(secs).ok())
}

/// `retry-after` when given, else exponential backoff; never above [`MAX_RETRY_DELAY`].
fn retry_delay(base: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or_else(|| base.saturating_mul(2u32.saturating_pow(attempt))).min(MAX_RETRY_DELAY)
}

/// Error body: `{"type":"error","error":{"type","message"}}`.
#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

/// Human-readable description of an error response.
fn describe_error(status: u16, body: &str) -> String {
    match serde_json::from_str::<ErrorBody>(body) {
        Ok(parsed) => format!("HTTP {status} {}: {}", parsed.error.kind, parsed.error.message),
        Err(_) => {
            let snippet: String = body.chars().take(MAX_RAW_ERROR_CHARS).collect();
            format!("HTTP {status}: {}", snippet.trim())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_doubles_honours_retry_after_and_caps() {
        let base = Duration::from_secs(1);
        assert_eq!(retry_delay(base, 2, None), Duration::from_secs(4));
        assert_eq!(retry_delay(base, 0, Some(Duration::from_secs(7))), Duration::from_secs(7));
        assert_eq!(retry_delay(base, 0, Some(Duration::from_secs(600))), MAX_RETRY_DELAY);
        assert_eq!(parse_retry_after("abc".into()), None);
    }

    #[test]
    fn classifies_statuses() {
        let fatal = |status: u16, should_retry| match classify_status(
            StatusCode::from_u16(status).unwrap(),
            should_retry,
            None,
            "r".into(),
        ) {
            Failure::Fatal(error) => Some(error),
            Failure::Retry { .. } => None,
        };
        assert_eq!(fatal(529, None), None);
        assert_eq!(fatal(408, None), None);
        assert_eq!(fatal(413, None), Some(LlmError::Rejected("r".into())));
        assert_eq!(fatal(503, Some(false)), Some(LlmError::Unavailable("r".into())));
        assert_eq!(fatal(400, Some(true)), None);
    }

    #[test]
    fn key_problems_are_rejected_without_echoing_the_key() {
        let config = ClaudeConfig::default();
        assert!(matches!(Transport::new("  ", &config), Err(LlmError::Rejected(_))));
        match Transport::new("bad\nkey-secret", &config) {
            Err(LlmError::Rejected(reason)) => assert!(!reason.contains("secret"), "{reason}"),
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn error_descriptions() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(describe_error(529, body), "HTTP 529 overloaded_error: Overloaded");
        assert_eq!(describe_error(502, " <html>bad gateway</html> "), "HTTP 502: <html>bad gateway</html>");
    }
}
