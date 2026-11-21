//! Prompt building blocks.

/// One system prompt block. `cache = true` puts an ephemeral cache breakpoint on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBlock {
    pub text: String,
    pub cache: bool,
}

/// One part of the user message. Images are sent before text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentPart {
    Text(String),
    /// Base64-encoded image, e.g. `media_type = "image/png"`.
    Image {
        media_type: String,
        base64: String,
    },
}
