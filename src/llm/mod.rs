mod application;
mod domain;
mod infrastructure;

#[cfg(test)]
pub use application::FakeModel;
pub use application::{MeteredModel, StructuredModel, UsageMeter};
pub use domain::error::{LlmError, USAGE_LIMIT_PREFIX};
pub use domain::message::{ContentPart, Structured, SystemBlock};
pub use domain::tools::{LoopLimits, LoopOutcome, LoopStop, ToolHandler, ToolOutput, ToolSpec};
pub use domain::usage::Usage;
pub(crate) use infrastructure::cli::classify_failure as classify_cli_failure;
pub use infrastructure::cli::{CliClient, CliConfig, estimate_input_tokens, tokens_for_chars};
