//! Messages API wire format: request body and response envelope.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::review::application::ports::ReviewerError;
use crate::review::domain::report::TokenUsage;

const STOP_REFUSAL: &str = "refusal";
const STOP_MAX_TOKENS: &str = "max_tokens";

/// Everything that varies between the review and the assessment request.
pub struct MessageRequest<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub effort: &'a str,
    pub use_fallbacks: bool,
    /// System text blocks; `(text, cached)` where `cached` adds an ephemeral cache breakpoint.
    pub system: Vec<(&'a str, bool)>,
    pub user_text: &'a str,
    pub schema: Value,
}

impl MessageRequest<'_> {
    /// JSON body for `POST /v1/messages`.
    pub fn to_body(&self) -> Value {
        let system: Vec<Value> = self
            .system
            .iter()
            .map(|(text, cached)| {
                let mut block = json!({ "type": "text", "text": text });
                if *cached {
                    block["cache_control"] = json!({ "type": "ephemeral" });
                }
                block
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "thinking": { "type": "adaptive" },
            "output_config": {
                "effort": self.effort,
                "format": { "type": "json_schema", "schema": self.schema },
            },
            "system": system,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": self.user_text }] }],
        });
        if self.use_fallbacks {
            body["fallbacks"] = json!("default");
        }
        body
    }
}

/// Successful Messages API response (only the fields qgate uses).
#[derive(Debug, Deserialize)]
pub struct MessageResponse {
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_details: Option<StopDetails>,
    pub usage: Usage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Thinking, fallback and any future block types carry nothing qgate needs.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct StopDetails {
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
}

impl From<&Usage> for TokenUsage {
    fn from(u: &Usage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens.unwrap_or(0),
            cache_write_tokens: u.cache_creation_input_tokens.unwrap_or(0),
        }
    }
}

impl MessageResponse {
    /// Checks the stop reason, then parses the concatenated text blocks as `T`.
    pub fn parse_answer<T: DeserializeOwned>(&self) -> Result<T, ReviewerError> {
        match self.stop_reason.as_deref() {
            Some(STOP_REFUSAL) => {
                let category = self.stop_details.as_ref().and_then(|d| d.category.clone());
                return Err(ReviewerError::Refused { category });
            }
            Some(STOP_MAX_TOKENS) => return Err(ReviewerError::Truncated),
            _ => {}
        }
        let text: String = self
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect();
        if text.trim().is_empty() {
            return Err(ReviewerError::InvalidResponse("response contained no text".into()));
        }
        serde_json::from_str(&text)
            .map_err(|e| ReviewerError::InvalidResponse(format!("answer does not match the schema: {e}")))
    }
}

/// Error body: `{"type":"error","error":{"type","message"}}`.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

const MAX_RAW_ERROR_CHARS: usize = 300;

/// Human-readable description of an error response.
pub fn describe_error(status: u16, body: &str) -> String {
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

    fn response(stop_reason: &str, content: Value) -> MessageResponse {
        serde_json::from_value(json!({
            "model": "claude-opus-5-5", "content": content, "stop_reason": stop_reason,
            "stop_details": null, "usage": {"input_tokens": 1, "output_tokens": 2}
        }))
        .unwrap()
    }

    #[test]
    fn body_omits_fallbacks_and_cache_when_disabled() {
        let request = MessageRequest {
            model: "m",
            max_tokens: 10,
            effort: "low",
            use_fallbacks: false,
            system: vec![("s", false)],
            user_text: "u",
            schema: json!({}),
        };
        let body = request.to_body();
        assert!(body.get("fallbacks").is_none());
        assert!(body["system"][0].get("cache_control").is_none());
        assert_eq!(body["output_config"]["effort"], "low");
    }

    #[test]
    fn empty_text_is_invalid() {
        let r = response("end_turn", json!([{"type": "thinking", "thinking": ""}]));
        assert!(matches!(r.parse_answer::<Value>(), Err(ReviewerError::InvalidResponse(_))));
    }

    #[test]
    fn missing_cache_usage_defaults_to_zero() {
        let r = response("end_turn", json!([]));
        let usage = TokenUsage::from(&r.usage);
        assert_eq!((usage.input_tokens, usage.output_tokens, usage.cache_read_tokens), (1, 2, 0));
    }

    #[test]
    fn error_descriptions() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(describe_error(529, body), "HTTP 529 overloaded_error: Overloaded");
        assert_eq!(describe_error(502, " <html>bad gateway</html> "), "HTTP 502: <html>bad gateway</html>");
    }
}
