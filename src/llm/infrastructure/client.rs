//! Generic Claude Messages API client: structured output and an agentic tool loop.

use futures::future::join_all;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::config::ClaudeConfig;
use super::transport::Transport;
use super::wire::{
    ContentBlock, Message, MessageResponse, RequestBody, STOP_PAUSE_TURN, STOP_TOOL_USE, adaptive_thinking,
    output_config, system_blocks, text_of, tool_definitions, tool_result,
};
use crate::llm::{
    Budget, ContentPart, LlmError, LoopLimits, LoopOutcome, LoopStop, SystemBlock, ToolHandler, ToolOutput, ToolSpec,
    Usage,
};

const FALLBACKS_DEFAULT: &str = "default";
/// `pause_turn` continuations accepted by [`ClaudeClient::structured`] before giving up.
const MAX_STRUCTURED_RESUMES: u32 = 4;

/// Calls Claude. Deliberately not `Debug`: its HTTP client carries the API key.
pub struct ClaudeClient {
    transport: Transport,
    config: ClaudeConfig,
    budget: Budget,
}

/// The parts of a request that stay fixed for a whole conversation.
struct Conversation {
    system: Vec<Value>,
    tools: Vec<Value>,
    output_config: Value,
    messages: Vec<Message>,
}

struct ToolCall {
    id: String,
    name: String,
    input: Value,
}

impl ClaudeClient {
    /// Builds the client. Fails with `Rejected` on an empty/invalid key or invalid settings.
    pub fn new(api_key: String, config: ClaudeConfig, budget: Budget) -> Result<Self, LlmError> {
        config.validate()?;
        let transport = Transport::new(&api_key, &config)?;
        Ok(Self { transport, config, budget })
    }

    /// One request with `output_config.format` json_schema; parses the text into `T`.
    pub async fn structured<T: DeserializeOwned>(
        &self,
        system: &[SystemBlock],
        user: Vec<ContentPart>,
        schema: Value,
    ) -> Result<T, LlmError> {
        let mut conversation = Conversation {
            system: system_blocks(system),
            tools: Vec::new(),
            output_config: output_config(&self.config.effort, Some(schema)),
            messages: vec![Message::user(user)?],
        };
        let mut text = String::new();
        for _ in 0..=MAX_STRUCTURED_RESUMES {
            let response = self.send(&conversation).await?;
            text.push_str(&text_of(&response.blocks()?));
            if response.stop_reason.as_deref() != Some(STOP_PAUSE_TURN) {
                return parse_answer(&text);
            }
            conversation.messages.push(Message::assistant(response.content));
        }
        Err(LlmError::InvalidResponse(format!("still paused after {MAX_STRUCTURED_RESUMES} continuations")))
    }

    /// Agentic loop with client tools.
    ///
    /// Each assistant turn is appended verbatim; all `tool_result`s of a turn go back in one user message,
    /// in `tool_use` order. Stops when the handler reports `finished`, Claude stops calling tools, or
    /// `limits.max_turns` requests were sent.
    pub async fn run_tools<H: ToolHandler>(
        &self,
        system: &[SystemBlock],
        user: Vec<ContentPart>,
        tools: &[ToolSpec],
        handler: &H,
        limits: LoopLimits,
    ) -> Result<LoopOutcome, LlmError> {
        let mut conversation = Conversation {
            system: system_blocks(system),
            tools: tool_definitions(tools),
            output_config: output_config(&self.config.effort, None),
            messages: vec![Message::user(user)?],
        };
        let mut final_text = String::new();
        let mut turns = 0;
        while turns < limits.max_turns {
            let response = self.send(&conversation).await?;
            turns += 1;
            let blocks = response.blocks()?;
            let text = text_of(&blocks);
            if !text.trim().is_empty() {
                final_text = text;
            }
            let stop_reason = response.stop_reason.clone();
            conversation.messages.push(Message::assistant(response.content));
            match stop_reason.as_deref() {
                Some(STOP_PAUSE_TURN) => {}
                Some(STOP_TOOL_USE) => {
                    let results = run_calls(handler, tools, tool_calls(blocks)?).await;
                    if handler.finished() {
                        return Ok(LoopOutcome { turns, stop: LoopStop::Finished, final_text });
                    }
                    conversation.messages.push(Message::tool_results(results));
                }
                _ => {
                    let stop = if handler.finished() { LoopStop::Finished } else { LoopStop::NoMoreToolCalls };
                    return Ok(LoopOutcome { turns, stop, final_text });
                }
            }
        }
        Ok(LoopOutcome { turns, stop: LoopStop::MaxTurns, final_text })
    }

    /// Sends the conversation once (with retries), records its usage and checks the stop reason.
    async fn send(&self, conversation: &Conversation) -> Result<MessageResponse, LlmError> {
        if self.budget.remaining_usd() <= 0.0 {
            return Err(LlmError::BudgetExceeded { limit_usd: self.budget.limit_usd() });
        }
        let body = RequestBody {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            thinking: adaptive_thinking(),
            output_config: &conversation.output_config,
            system: &conversation.system,
            tools: &conversation.tools,
            messages: &conversation.messages,
            fallbacks: self.config.use_fallbacks.then_some(FALLBACKS_DEFAULT),
        };
        let body = serde_json::to_string(&body)
            .map_err(|e| LlmError::Rejected(format!("cannot serialise the request: {e}")))?;
        let text = self.transport.post(&body).await?;
        let response: MessageResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("unexpected response envelope: {e}")))?;
        self.budget.record(&response.model, &Usage::from(&response.usage));
        response.check_stop()?;
        Ok(response)
    }
}

fn parse_answer<T: DeserializeOwned>(text: &str) -> Result<T, LlmError> {
    if text.trim().is_empty() {
        return Err(LlmError::InvalidResponse("response contained no text".into()));
    }
    serde_json::from_str(text).map_err(|e| LlmError::InvalidResponse(format!("answer does not match the schema: {e}")))
}

fn tool_calls(blocks: Vec<ContentBlock>) -> Result<Vec<ToolCall>, LlmError> {
    let calls: Vec<ToolCall> = blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(ToolCall { id, name, input }),
            _ => None,
        })
        .collect();
    if calls.is_empty() {
        return Err(LlmError::InvalidResponse("stop_reason tool_use without any tool_use block".into()));
    }
    Ok(calls)
}

/// Runs all calls of one turn concurrently; results come back in call order.
async fn run_calls<H: ToolHandler>(handler: &H, tools: &[ToolSpec], calls: Vec<ToolCall>) -> Vec<Value> {
    let runs = calls.into_iter().map(|call| async move {
        let output = if !tools.iter().any(|t| t.name == call.name) {
            ToolOutput::error(format!("unknown tool {:?}", call.name))
        } else if !call.input.is_object() {
            ToolOutput::error("tool input must be a JSON object")
        } else {
            handler.call(&call.name, call.input).await
        };
        tool_result(&call.id, output)
    });
    join_all(runs).await
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
