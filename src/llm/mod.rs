//! Shared Claude client kernel: one generic Messages API client (structured output and an
//! agentic tool loop) plus a spend tracker shared by every agent of a review.
//!
//! `domain` holds the pure parts (usage, budget, pricing, tool contracts, errors);
//! `infrastructure` holds the raw-HTTP adapter.

mod domain;
mod infrastructure;

pub use domain::budget::{Budget, Usage};
pub use domain::error::LlmError;
pub use domain::message::{ContentPart, SystemBlock};
pub use domain::pricing::estimate_cost_usd;
pub use domain::tools::{LoopLimits, LoopOutcome, LoopStop, ToolHandler, ToolOutput, ToolSpec};
pub use infrastructure::client::ClaudeClient;
pub use infrastructure::config::ClaudeConfig;
