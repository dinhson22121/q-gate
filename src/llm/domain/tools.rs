use std::future::Future;

use serde_json::Value;

use super::usage::Usage;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

pub trait ToolHandler: Send + Sync {
    fn call(&self, name: &str, input: Value) -> impl Future<Output = ToolOutput> + Send;
    fn finished(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopLimits {
    pub max_turns: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStop {
    Finished,
    NoMoreToolCalls,
    MaxTurns,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoopOutcome {
    pub turns: u32,
    pub stop: LoopStop,
    pub final_text: String,
    pub usage: Usage,
    pub cost_usd: f64,
}
