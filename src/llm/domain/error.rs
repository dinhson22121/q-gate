//! Errors of a Claude call. Messages never contain the API key.

/// Why a Claude call failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    #[error("Claude stopped at max_tokens before finishing its answer")]
    Truncated,
    #[error("Claude refused to answer (category: {})", category.as_deref().unwrap_or("unspecified"))]
    Refused { category: Option<String> },
    #[error("invalid response from Claude: {0}")]
    InvalidResponse(String),
    #[error("Claude API unavailable: {0}")]
    Unavailable(String),
    #[error("Claude API rejected the request: {0}")]
    Rejected(String),
    #[error("LLM budget of ${limit_usd:.2} exhausted")]
    BudgetExceeded { limit_usd: f64 },
}
