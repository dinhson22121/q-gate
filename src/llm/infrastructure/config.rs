//! Tunables of the Claude client. Holds no secret.

use std::time::Duration;

use crate::llm::LlmError;

const VALID_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeConfig {
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

impl Default for ClaudeConfig {
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

impl ClaudeConfig {
    pub(super) fn validate(&self) -> Result<(), LlmError> {
        let invalid = |what: String| Err(LlmError::Rejected(format!("invalid Claude settings: {what}")));
        let Some(rest) = self.base_url.strip_prefix("https://").or_else(|| self.base_url.strip_prefix("http://"))
        else {
            return invalid(format!("base_url must be an http(s) URL, got {:?}", self.base_url));
        };
        if rest.split(['/', '?', '#']).next().unwrap_or_default().contains('@') {
            return invalid("base_url must not contain credentials (user@host)".into());
        }
        if self.model.trim().is_empty() {
            return invalid("model is empty".into());
        }
        if !VALID_EFFORTS.contains(&self.effort.as_str()) {
            return invalid(format!("effort must be one of {VALID_EFFORTS:?}, got {:?}", self.effort));
        }
        if self.max_tokens == 0 {
            return invalid("max_tokens must be positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_with_userinfo_is_rejected() {
        let config = ClaudeConfig { base_url: "http://localhost:x@evil.example".into(), ..ClaudeConfig::default() };
        assert!(config.validate().is_err());
    }

    fn rejected(config: ClaudeConfig) -> String {
        match config.validate() {
            Err(LlmError::Rejected(reason)) => reason,
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn default_is_valid() {
        assert_eq!(ClaudeConfig::default().validate(), Ok(()));
    }

    #[test]
    fn rejects_each_invalid_field() {
        let base = ClaudeConfig::default;
        assert!(rejected(ClaudeConfig { base_url: "ftp://x".into(), ..base() }).contains("base_url"));
        assert!(rejected(ClaudeConfig { model: " ".into(), ..base() }).contains("model"));
        assert!(rejected(ClaudeConfig { effort: "huge".into(), ..base() }).contains("effort"));
        assert!(rejected(ClaudeConfig { max_tokens: 0, ..base() }).contains("max_tokens"));
    }
}
