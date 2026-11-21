//! Figma REST adapter (personal access token in `X-Figma-Token`).
//!
//! Figma's cloud MCP server only accepts OAuth from catalog-listed clients, so a background service
//! reads the REST API: `/v1/files/{key}/nodes`, `/variables/local` (Enterprise only; 403 → node values)
//! and `/v1/images/{key}` (PNG render URL, downloaded only from Figma's own hosts).

use std::fmt;
use std::time::Duration;

use futures::future::BoxFuture;
use reqwest::{Response, StatusCode, Url};
use serde_json::Value;

use super::figma_parse::{parse_nodes, parse_variables, resolve_tokens, version_of};
use crate::design::application::{DesignError, FetchedDesign, FigmaLink, FigmaSource};
use crate::design::domain::link::{is_valid_file_key, is_valid_node_id};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Retries after HTTP 429 (each honours `Retry-After`).
const MAX_RETRIES: u32 = 2;
/// Longest `Retry-After` we wait for; longer ones fail the call instead of stalling the review.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);
const DEFAULT_RETRY_WAIT: Duration = Duration::from_secs(2);
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const PNG_MAGIC: [u8; 4] = [0x89, b'P', b'N', b'G'];
const LOCAL_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// Figma personal access token. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct FigmaToken(String);

impl FigmaToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Debug for FigmaToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FigmaToken(***)")
    }
}

#[derive(Debug, Clone)]
pub struct FigmaRestConfig {
    /// e.g. `https://api.figma.com` (https required; http only for localhost).
    pub api_base_url: String,
    pub token: FigmaToken,
}

/// Reads frames from the Figma REST API. Not `Debug`-printable with the token.
pub struct FigmaRestSource {
    api: reqwest::Client,
    images: reqwest::Client,
    base: Url,
    token: FigmaToken,
}

impl fmt::Debug for FigmaRestSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FigmaRestSource").field("base", &self.base.as_str()).finish_non_exhaustive()
    }
}

impl FigmaRestSource {
    pub fn new(config: FigmaRestConfig) -> Result<Self, DesignError> {
        let base = validate_base_url(&config.api_base_url)?;
        if config.token.0.trim().is_empty() {
            return Err(DesignError::Config("FIGMA_TOKEN is not set".into()));
        }
        let build = || {
            reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                // Never follow redirects: the token must only reach the configured API host, and images
                // are only downloaded from the allow-listed host Figma returned.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| DesignError::Config(format!("cannot build HTTP client: {e}")))
        };
        Ok(Self { api: build()?, images: build()?, base, token: FigmaToken(config.token.0.trim().to_string()) })
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, DesignError> {
        let mut url = self.base.join(path).map_err(|e| DesignError::Config(format!("invalid API path: {e}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// GET with the token; honours `Retry-After` on 429 up to `MAX_RETRIES` times.
    async fn get(&self, url: Url) -> Result<Response, DesignError> {
        let mut retries = 0;
        loop {
            let response = self
                .api
                .get(url.clone())
                .header("X-Figma-Token", &self.token.0)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| self.transport_error(&e))?;
            if response.status() != StatusCode::TOO_MANY_REQUESTS {
                return Ok(response);
            }
            let wait = retry_after(&response);
            if retries >= MAX_RETRIES || wait > MAX_RETRY_WAIT {
                return Err(DesignError::Unavailable(format!(
                    "rate limited (HTTP 429, retry after {}s) after {retries} retries",
                    wait.as_secs()
                )));
            }
            retries += 1;
            tokio::time::sleep(wait).await;
        }
    }

    async fn get_json(&self, url: Url, what: &str) -> Result<Value, DesignError> {
        let response = self.get(url).await?;
        check_status(response.status(), what)?;
        response.json().await.map_err(|e| DesignError::Invalid(format!("{what} JSON: {}", self.redact(&e))))
    }

    /// Local variables; `None` when the plan does not allow the endpoint (403) or it is missing.
    async fn variables(&self, file_key: &str) -> Result<Option<Value>, DesignError> {
        let response = self.get(self.url(&format!("v1/files/{file_key}/variables/local"), &[])?).await?;
        match response.status() {
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Ok(None),
            status => {
                check_status(status, "variables")?;
                let body = response
                    .json()
                    .await
                    .map_err(|e| DesignError::Invalid(format!("variables JSON: {}", self.redact(&e))))?;
                Ok(Some(body))
            }
        }
    }

    async fn png(&self, link: &FigmaLink) -> Result<Option<Vec<u8>>, DesignError> {
        let query = [("ids", link.node_id.as_str()), ("format", "png"), ("scale", "1")];
        let body = self.get_json(self.url(&format!("v1/images/{}", link.file_key), &query)?, "images").await?;
        if let Some(error) = body.get("err").and_then(Value::as_str) {
            return Err(DesignError::Invalid(format!("image render failed: {error}")));
        }
        let Some(raw) = body.get("images").and_then(|i| i.get(&link.node_id)).and_then(Value::as_str) else {
            return Ok(None);
        };
        let url = Url::parse(raw).map_err(|_| DesignError::Invalid("image URL is not a valid URL".into()))?;
        if !self.is_allowed_image_host(&url) {
            return Err(DesignError::Invalid(format!(
                "image host {} is not an allowed Figma host; image skipped",
                url.host_str().unwrap_or("?")
            )));
        }
        self.download(url).await.map(Some)
    }

    /// Only Figma's own hosts over https (or the API's own origin for local test servers).
    fn is_allowed_image_host(&self, url: &Url) -> bool {
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let figma = host == "figma.com" || host.ends_with(".figma.com");
        let s3 = host.starts_with("figma-alpha-api.s3") && host.ends_with(".amazonaws.com");
        let same_local_origin = self.base.origin() == url.origin() && is_local(&self.base);
        (url.scheme() == "https" && (figma || s3)) || same_local_origin
    }

    /// Downloads without the token, capped at `MAX_IMAGE_BYTES`, and checks it is a PNG.
    async fn download(&self, url: Url) -> Result<Vec<u8>, DesignError> {
        let mut response = self.images.get(url).send().await.map_err(|e| self.transport_error(&e))?;
        check_status(response.status(), "image download")?;
        if response.content_length().is_some_and(|len| len > MAX_IMAGE_BYTES as u64) {
            return Err(DesignError::Invalid("frame image is larger than 5 MB; image skipped".into()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| self.transport_error(&e))? {
            if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
                return Err(DesignError::Invalid("frame image is larger than 5 MB; image skipped".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !bytes.starts_with(&PNG_MAGIC) {
            return Err(DesignError::Invalid("frame image is not a PNG; image skipped".into()));
        }
        Ok(bytes)
    }

    async fn fetch_design(&self, link: &FigmaLink, with_png: bool) -> Result<FetchedDesign, DesignError> {
        validate_link(link)?;
        let query = [("ids", link.node_id.as_str()), ("geometry", "omit")];
        let url = self.url(&format!("v1/files/{}/nodes", link.file_key), &query)?;
        let mut parsed = parse_nodes(&self.get_json(url, "nodes").await?, &link.node_id)?;
        let mut notes = Vec::new();
        if parsed.truncated {
            notes.push("frame is very large; only part of it was read".to_string());
        }
        let variables = if parsed.bound.is_empty() { None } else { self.variables(&link.file_key).await? };
        if variables.is_none() && !parsed.bound.is_empty() {
            notes.push("variables API not available (non-Enterprise plan?); using resolved node values".into());
        }
        resolve_tokens(&mut parsed, variables.as_ref().map(parse_variables).as_ref());
        let png = if with_png {
            self.png(link).await.unwrap_or_else(|error| {
                notes.push(error.to_string());
                None
            })
        } else {
            None
        };
        Ok(FetchedDesign { spec: parsed.spec, version: parsed.version, png, notes })
    }

    async fn version(&self, file_key: &str) -> Result<String, DesignError> {
        if !is_valid_file_key(file_key) {
            return Err(DesignError::Config("invalid Figma file key".into()));
        }
        let body = self.get_json(self.url(&format!("v1/files/{file_key}"), &[("depth", "1")])?, "file").await?;
        Ok(version_of(&body))
    }

    fn redact(&self, error: &reqwest::Error) -> String {
        error_text(&self.token, error)
    }

    fn transport_error(&self, error: &reqwest::Error) -> DesignError {
        let kind = if error.is_timeout() {
            "timed out"
        } else if error.is_connect() {
            "connection failed"
        } else {
            "request failed"
        };
        DesignError::Unavailable(format!("{kind}: {}", self.redact(error)))
    }
}

impl FigmaSource for FigmaRestSource {
    fn file_version<'a>(&'a self, file_key: &'a str) -> BoxFuture<'a, Result<String, DesignError>> {
        Box::pin(self.version(file_key))
    }

    fn fetch<'a>(&'a self, link: &'a FigmaLink, with_png: bool) -> BoxFuture<'a, Result<FetchedDesign, DesignError>> {
        Box::pin(self.fetch_design(link, with_png))
    }
}

/// Error text without URL and without the token (in case a server echoes it).
fn error_text(token: &FigmaToken, error: &reqwest::Error) -> String {
    let mut text = format!("{error}");
    if let Some(url) = error.url() {
        text = text.replace(url.as_str(), "<url>");
    }
    if token.0.is_empty() { text } else { text.replace(&token.0, "***") }
}

fn validate_link(link: &FigmaLink) -> Result<(), DesignError> {
    if is_valid_file_key(&link.file_key) && is_valid_node_id(&link.node_id) {
        Ok(())
    } else {
        Err(DesignError::Config("invalid Figma file key or node id".into()))
    }
}

fn is_local(url: &Url) -> bool {
    url.scheme() == "http" && LOCAL_HOSTS.contains(&url.host_str().unwrap_or_default())
}

/// Requires https (http only for localhost); the result ends with `/` so `join` keeps any path prefix.
fn validate_base_url(raw: &str) -> Result<Url, DesignError> {
    let url = Url::parse(raw.trim()).map_err(|e| DesignError::Config(format!("invalid Figma API URL: {e}")))?;
    if url.scheme() != "https" && !is_local(&url) {
        return Err(DesignError::Config("Figma API URL must use https".into()));
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(DesignError::Config("Figma API URL must not contain credentials, query or fragment".into()));
    }
    let with_slash = format!("{}/", url.as_str().trim_end_matches('/'));
    Url::parse(&with_slash).map_err(|e| DesignError::Config(format!("invalid Figma API URL: {e}")))
}

fn retry_after(response: &Response) -> Duration {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(DEFAULT_RETRY_WAIT, Duration::from_secs)
}

fn check_status(status: StatusCode, what: &str) -> Result<(), DesignError> {
    match status {
        s if s.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(DesignError::Unauthorized(format!(
            "{what}: HTTP {status}; check FIGMA_TOKEN and that it can read this file"
        ))),
        StatusCode::NOT_FOUND => Err(DesignError::NotFound(format!("{what}: HTTP 404"))),
        s if s.is_redirection() => Err(DesignError::Invalid(format!(
            "{what}: unexpected redirect (HTTP {status}); redirects are not followed"
        ))),
        s if s.is_server_error() => Err(DesignError::Unavailable(format!("{what}: HTTP {status}"))),
        _ => Err(DesignError::Invalid(format!("{what}: HTTP {status}"))),
    }
}

#[cfg(test)]
#[path = "figma_rest_tests.rs"]
mod tests;
