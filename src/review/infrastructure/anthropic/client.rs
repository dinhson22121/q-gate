//! `CodeReviewer` adapter calling the Claude Messages API over raw HTTP.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;

use super::api::{MessageRequest, MessageResponse, describe_error};
use super::pricing::estimate_cost_usd;
use super::prompt::{
    REQUIREMENTS_SYSTEM_PROMPT, REVIEW_SYSTEM_PROMPT, guidelines_block, requirements_user_message, review_user_message,
};
use super::schema::{AssessmentAnswer, ReviewAnswer, assessment_schema, review_schema};
use crate::review::application::ports::{
    AssessmentOutput, CodeReviewer, RequirementsInput, ReviewInput, ReviewerError, ReviewerOutput,
};
use crate::review::domain::report::TokenUsage;

const MESSAGES_PATH: &str = "/v1/messages";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const HEADER_API_KEY: &str = "x-api-key";
const HEADER_VERSION: &str = "anthropic-version";
const HEADER_BETA: &str = "anthropic-beta";
const HEADER_SHOULD_RETRY: &str = "x-should-retry";
const HEADER_RETRY_AFTER: &str = "retry-after";
/// Retries after the first attempt.
const MAX_RETRIES: u32 = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const VALID_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Tunables of the Anthropic adapter. Holds no secret.
#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicSettings {
    pub base_url: String,
    pub model: String,
    /// `output_config.effort`: low, medium, high, xhigh or max.
    pub effort: String,
    pub max_tokens: u32,
    /// Opt into server-side fallback to another model when the primary model refuses.
    pub use_fallbacks: bool,
    pub request_timeout: Duration,
    /// First retry delay; doubles on every further retry.
    pub retry_base_delay: Duration,
}

impl Default for AnthropicSettings {
    fn default() -> Self {
        Self {
            base_url: "https://api.anthropic.com".into(),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            max_tokens: 16_000,
            use_fallbacks: true,
            request_timeout: Duration::from_secs(600),
            retry_base_delay: Duration::from_secs(1),
        }
    }
}

/// Reviews code with Claude. Deliberately not `Debug`: its HTTP client carries the API key.
pub struct AnthropicReviewer {
    http: reqwest::Client,
    endpoint: String,
    settings: AnthropicSettings,
}

/// Outcome of a single failed attempt.
enum Failure {
    Retry { reason: String, retry_after: Option<Duration> },
    Fatal(ReviewerError),
}

impl AnthropicReviewer {
    /// Builds the reviewer. Fails with `Rejected` on an empty/invalid key or invalid settings.
    pub fn new(api_key: String, settings: AnthropicSettings) -> Result<Self, ReviewerError> {
        validate(&settings)?;
        let http = reqwest::Client::builder()
            .default_headers(default_headers(api_key.trim(), settings.use_fallbacks)?)
            .timeout(settings.request_timeout)
            // Never follow redirects: reqwest would replay the custom `x-api-key` header to the new host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ReviewerError::Rejected(format!("cannot build HTTP client: {e}")))?;
        let endpoint = format!("{}{MESSAGES_PATH}", settings.base_url.trim_end_matches('/'));
        Ok(Self { http, endpoint, settings })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        request: &MessageRequest<'_>,
    ) -> Result<(T, TokenUsage, Option<f64>), ReviewerError> {
        let response = self.send_with_retry(&request.to_body()).await?;
        let answer = response.parse_answer()?;
        let usage = TokenUsage::from(&response.usage);
        Ok((answer, usage, estimate_cost_usd(&response.model, &usage)))
    }

    async fn send_with_retry(&self, body: &serde_json::Value) -> Result<MessageResponse, ReviewerError> {
        let mut attempt = 0;
        loop {
            match self.send_once(body).await {
                Ok(response) => return Ok(response),
                Err(Failure::Fatal(error)) => return Err(error),
                Err(Failure::Retry { reason, .. }) if attempt >= MAX_RETRIES => {
                    return Err(ReviewerError::Unavailable(reason));
                }
                Err(Failure::Retry { retry_after, .. }) => {
                    tokio::time::sleep(retry_delay(self.settings.retry_base_delay, attempt, retry_after)).await;
                    attempt += 1;
                }
            }
        }
    }

    async fn send_once(&self, body: &serde_json::Value) -> Result<MessageResponse, Failure> {
        let response = self.http.post(&self.endpoint).json(body).send().await.map_err(transport_failure)?;
        let status = response.status();
        if status.is_success() {
            let text = response.text().await.map_err(transport_failure)?;
            return serde_json::from_str(&text).map_err(|e| {
                Failure::Fatal(ReviewerError::InvalidResponse(format!("unexpected response envelope: {e}")))
            });
        }
        if status.is_redirection() {
            return Err(Failure::Fatal(ReviewerError::Rejected(format!(
                "unexpected redirect (HTTP {}) from the Anthropic API; redirects are not followed, check llm.base_url",
                status.as_u16()
            ))));
        }
        Err(status_failure(status, response).await)
    }
}

fn validate(settings: &AnthropicSettings) -> Result<(), ReviewerError> {
    let invalid = |what: String| Err(ReviewerError::Rejected(format!("invalid Anthropic settings: {what}")));
    if !settings.base_url.starts_with("https://") && !settings.base_url.starts_with("http://") {
        return invalid(format!("base_url must be an http(s) URL, got {:?}", settings.base_url));
    }
    if settings.model.trim().is_empty() {
        return invalid("model is empty".into());
    }
    if !VALID_EFFORTS.contains(&settings.effort.as_str()) {
        return invalid(format!("effort must be one of {VALID_EFFORTS:?}, got {:?}", settings.effort));
    }
    if settings.max_tokens == 0 {
        return invalid("max_tokens must be positive".into());
    }
    Ok(())
}

fn default_headers(api_key: &str, use_fallbacks: bool) -> Result<HeaderMap, ReviewerError> {
    if api_key.is_empty() {
        return Err(ReviewerError::Rejected("Anthropic API key is empty".into()));
    }
    let mut key = HeaderValue::from_str(api_key)
        .map_err(|_| ReviewerError::Rejected("Anthropic API key contains invalid characters".into()))?;
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
        Failure::Fatal(ReviewerError::Unavailable(reason))
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
        Failure::Fatal(ReviewerError::Rejected(reason))
    } else {
        Failure::Fatal(ReviewerError::Unavailable(reason))
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

impl CodeReviewer for AnthropicReviewer {
    async fn review(&self, input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
        let guidelines = guidelines_block(input.guidelines);
        let user_text = review_user_message(input.files);
        let request = MessageRequest {
            model: &self.settings.model,
            max_tokens: self.settings.max_tokens,
            effort: &self.settings.effort,
            use_fallbacks: self.settings.use_fallbacks,
            system: vec![(REVIEW_SYSTEM_PROMPT, false), (&guidelines, true)],
            user_text: &user_text,
            schema: review_schema(),
        };
        let (answer, usage, cost_usd) = self.call::<ReviewAnswer>(&request).await?;
        Ok(ReviewerOutput { findings: answer.into_findings(), usage, cost_usd })
    }

    async fn assess_requirements(&self, input: RequirementsInput<'_>) -> Result<AssessmentOutput, ReviewerError> {
        let user_text = requirements_user_message(input.requirements, input.files);
        let request = MessageRequest {
            model: &self.settings.model,
            max_tokens: self.settings.max_tokens,
            effort: &self.settings.effort,
            use_fallbacks: self.settings.use_fallbacks,
            system: vec![(REQUIREMENTS_SYSTEM_PROMPT, false)],
            user_text: &user_text,
            schema: assessment_schema(),
        };
        let (answer, usage, cost_usd) = self.call::<AssessmentAnswer>(&request).await?;
        Ok(AssessmentOutput { assessments: answer.into_assessments(), usage, cost_usd })
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
