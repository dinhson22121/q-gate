pub const USAGE_LIMIT_PREFIX: &str = "Claude usage limit reached";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    #[error("Claude stopped at max_tokens before finishing its answer")]
    Truncated,
    #[error("Claude refused to answer (category: {})", category.as_deref().unwrap_or("unspecified"))]
    Refused { category: Option<String> },
    #[error("invalid response from Claude: {0}")]
    InvalidResponse(String),
    #[error("Claude unavailable: {0}")]
    Unavailable(String),
    #[error("Claude rejected the request: {0}")]
    Rejected(String),
    #[error("not supported by this model backend: {0}")]
    Unsupported(String),
    #[error("Claude usage limit reached{}: {message}", resets_at.as_deref().map(|t| format!(" (resets {t})")).unwrap_or_default())]
    RateLimited { resets_at: Option<String>, message: String },
}
