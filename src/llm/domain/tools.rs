//! Contracts of the agentic tool loop.

use std::future::Future;

use serde_json::Value;

/// A client tool offered to Claude.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the tool input (an object schema).
    pub input_schema: Value,
}

/// What a tool returned. `is_error = true` tells Claude the call failed.
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

/// Executes the tools Claude calls. Calls of one turn run concurrently.
pub trait ToolHandler: Send + Sync {
    /// Runs tool `name`; `input` is always a JSON object.
    fn call(&self, name: &str, input: Value) -> impl Future<Output = ToolOutput> + Send;
    /// Return true once the agent has reported its final result (loop then stops).
    fn finished(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopLimits {
    /// Maximum number of requests sent to Claude.
    pub max_turns: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStop {
    /// `ToolHandler::finished` returned true.
    Finished,
    /// Claude ended its turn without calling a tool.
    NoMoreToolCalls,
    /// `LoopLimits::max_turns` requests were sent.
    MaxTurns,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopOutcome {
    /// Requests sent to Claude.
    pub turns: u32,
    pub stop: LoopStop,
    /// Text of the last response that contained any.
    pub final_text: String,
}
