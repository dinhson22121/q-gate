//! Messages API wire format: request body, conversation history and response envelope.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::llm::{ContentPart, LlmError, SystemBlock, ToolOutput, ToolSpec, Usage};

pub(super) const STOP_REFUSAL: &str = "refusal";
pub(super) const STOP_MAX_TOKENS: &str = "max_tokens";
pub(super) const STOP_TOOL_USE: &str = "tool_use";
pub(super) const STOP_PAUSE_TURN: &str = "pause_turn";
const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// `POST /v1/messages` body. Assistant turns are embedded verbatim through [`RawValue`].
#[derive(Serialize)]
pub(super) struct RequestBody<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub thinking: Value,
    pub output_config: &'a Value,
    pub system: &'a [Value],
    #[serde(skip_serializing_if = "<[Value]>::is_empty")]
    pub tools: &'a [Value],
    pub messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallbacks: Option<&'static str>,
}

pub(super) fn adaptive_thinking() -> Value {
    json!({ "type": "adaptive" })
}

pub(super) fn output_config(effort: &str, schema: Option<Value>) -> Value {
    match schema {
        Some(schema) => json!({ "effort": effort, "format": { "type": "json_schema", "schema": schema } }),
        None => json!({ "effort": effort }),
    }
}

fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

pub(super) fn system_blocks(system: &[SystemBlock]) -> Vec<Value> {
    system
        .iter()
        .map(|block| {
            let mut value = json!({ "type": "text", "text": block.text });
            if block.cache {
                value["cache_control"] = ephemeral();
            }
            value
        })
        .collect()
}

/// Tool definitions; the last one carries a cache breakpoint so tools + system are cached across turns.
pub(super) fn tool_definitions(tools: &[ToolSpec]) -> Vec<Value> {
    let mut definitions: Vec<Value> = tools
        .iter()
        .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema }))
        .collect();
    if let Some(last) = definitions.last_mut() {
        last["cache_control"] = ephemeral();
    }
    definitions
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
    Assistant,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Content {
    Blocks(Vec<Value>),
    /// An assistant `content` array exactly as received (thinking signatures, fallback blocks...).
    Verbatim(Box<RawValue>),
}

/// One conversation message. History is append-only: messages are never edited once pushed.
#[derive(Serialize)]
pub(super) struct Message {
    role: Role,
    content: Content,
}

impl Message {
    /// The initial user message: images first, then text, each in the caller's order.
    pub(super) fn user(parts: Vec<ContentPart>) -> Result<Self, LlmError> {
        if parts.is_empty() {
            return Err(LlmError::Rejected("user message is empty".into()));
        }
        let (images, texts): (Vec<_>, Vec<_>) = parts.into_iter().partition(|p| matches!(p, ContentPart::Image { .. }));
        let blocks = images.into_iter().chain(texts).map(content_block).collect::<Result<_, _>>()?;
        Ok(Self { role: Role::User, content: Content::Blocks(blocks) })
    }

    pub(super) fn assistant(content: Box<RawValue>) -> Self {
        Self { role: Role::Assistant, content: Content::Verbatim(content) }
    }

    pub(super) fn tool_results(results: Vec<Value>) -> Self {
        Self { role: Role::User, content: Content::Blocks(results) }
    }
}

fn content_block(part: ContentPart) -> Result<Value, LlmError> {
    match part {
        ContentPart::Text(text) => Ok(json!({ "type": "text", "text": text })),
        ContentPart::Image { media_type, base64 } => {
            if !IMAGE_MEDIA_TYPES.contains(&media_type.as_str()) {
                return Err(LlmError::Rejected(format!(
                    "unsupported image media type {media_type:?}, expected one of {IMAGE_MEDIA_TYPES:?}"
                )));
            }
            if base64.is_empty() {
                return Err(LlmError::Rejected("image data is empty".into()));
            }
            Ok(json!({ "type": "image", "source": { "type": "base64", "media_type": media_type, "data": base64 } }))
        }
    }
}

pub(super) fn tool_result(tool_use_id: &str, output: ToolOutput) -> Value {
    json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": output.content, "is_error": output.is_error })
}

/// Successful Messages API response (only the fields qgate uses).
#[derive(Deserialize)]
pub(super) struct MessageResponse {
    pub model: String,
    /// Kept raw so it can be echoed back byte-identical in the next request.
    pub content: Box<RawValue>,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_details: Option<StopDetails>,
    pub usage: WireUsage,
}

#[derive(Deserialize)]
pub(super) struct StopDetails {
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct WireUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
}

impl From<&WireUsage> for Usage {
    fn from(u: &WireUsage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens.unwrap_or(0),
            cache_write_tokens: u.cache_creation_input_tokens.unwrap_or(0),
        }
    }
}

/// The content block kinds qgate reads; everything else (thinking, fallback...) is only echoed back.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(other)]
    Other,
}

impl MessageResponse {
    /// Fails on `refusal` / `max_tokens`, which must be checked before reading the content.
    pub(super) fn check_stop(&self) -> Result<(), LlmError> {
        match self.stop_reason.as_deref() {
            Some(STOP_REFUSAL) => {
                Err(LlmError::Refused { category: self.stop_details.as_ref().and_then(|d| d.category.clone()) })
            }
            Some(STOP_MAX_TOKENS) => Err(LlmError::Truncated),
            _ => Ok(()),
        }
    }

    pub(super) fn blocks(&self) -> Result<Vec<ContentBlock>, LlmError> {
        serde_json::from_str(self.content.get())
            .map_err(|e| LlmError::InvalidResponse(format!("unexpected content blocks: {e}")))
    }
}

/// Concatenated text of the text blocks.
pub(super) fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_puts_images_before_text() {
        let message = Message::user(vec![
            ContentPart::Text("look".into()),
            ContentPart::Image { media_type: "image/png".into(), base64: "AAAA".into() },
        ])
        .unwrap();
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["role"], "user");
        assert_eq!(value["content"][0]["source"], json!({"type": "base64", "media_type": "image/png", "data": "AAAA"}));
        assert_eq!(value["content"][1], json!({"type": "text", "text": "look"}));
    }

    #[test]
    fn rejects_empty_messages_and_bad_images() {
        assert!(matches!(Message::user(vec![]), Err(LlmError::Rejected(_))));
        let image = |media_type: &str, data: &str| {
            Message::user(vec![ContentPart::Image { media_type: media_type.into(), base64: data.into() }])
        };
        assert!(matches!(image("image/tiff", "AAAA"), Err(LlmError::Rejected(_))));
        assert!(matches!(image("image/png", ""), Err(LlmError::Rejected(_))));
    }

    #[test]
    fn no_tools_no_breakpoint_and_missing_cache_usage_is_zero() {
        assert!(tool_definitions(&[]).is_empty());
        let usage: WireUsage = serde_json::from_value(json!({"input_tokens": 1, "output_tokens": 2})).unwrap();
        assert_eq!(Usage::from(&usage), Usage { input_tokens: 1, output_tokens: 2, ..Usage::default() });
    }
}
