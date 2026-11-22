use std::pin::pin;
use std::process::Stdio;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::mcp_bridge::{RunningServer, SERVER_NAME, ToolBridge, ToolCall};
use crate::llm::{
    CliConfig, ContentPart, LlmError, LoopLimits, LoopOutcome, LoopStop, MAX_OUTPUT_BYTES, SystemBlock, ToolHandler,
    ToolSpec, Usage, envelope_consumption, exceeded_cap, read_capped,
};
use crate::testing::application::ports::ToolLoop;

const FINISH_GRACE: Duration = Duration::from_secs(20);
const OUTPUT_DRAIN: Duration = Duration::from_secs(2);
const CALL_QUEUE: usize = 16;

#[derive(Debug, Clone)]
pub struct CliToolLoop {
    config: CliConfig,
    finish_grace: Duration,
}

impl CliToolLoop {
    pub fn new(config: CliConfig) -> Result<Self, LlmError> {
        config.validate()?;
        Ok(Self { config, finish_grace: FINISH_GRACE })
    }

    #[cfg(test)]
    fn with_finish_grace(self, finish_grace: Duration) -> Self {
        Self { finish_grace, ..self }
    }

    fn args(&self, system: &str, config_path: &str, tools: &[ToolSpec]) -> Vec<String> {
        let allowed: Vec<String> = tools.iter().map(|t| format!("mcp__{SERVER_NAME}__{}", t.name)).collect();
        [
            "-p",
            "--output-format",
            "json",
            "--model",
            &self.config.model,
            "--effort",
            &self.config.effort,
            "--tools",
            "",
            "--mcp-config",
            config_path,
            "--strict-mcp-config",
            "--allowedTools",
            &allowed.join(","),
            "--append-system-prompt",
            system,
            "--setting-sources",
            "",
            "--disable-slash-commands",
            "--no-session-persistence",
        ]
        .map(String::from)
        .to_vec()
    }

    fn spawn(&self, args: Vec<String>, cwd: &std::path::Path) -> Result<Child, LlmError> {
        Command::new(&self.config.claude_bin)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| LlmError::Unavailable(format!("cannot start {}: {e}", self.config.claude_bin.display())))
    }
}

enum Ended {
    Finished,
    MaxTurns,
    Timeout,
    Exited,
}

impl ToolLoop for CliToolLoop {
    async fn drive<H: ToolHandler>(
        &self,
        system: &[SystemBlock],
        user: Vec<ContentPart>,
        tools: &[ToolSpec],
        handler: &H,
        limits: LoopLimits,
    ) -> Result<LoopOutcome, LlmError> {
        let (calls_tx, mut calls) = mpsc::channel(CALL_QUEUE);
        let server = RunningServer::start(ToolBridge::new(tools, calls_tx))
            .await
            .map_err(|e| LlmError::Unavailable(format!("cannot start the tool server: {e}")))?;
        let dir = tempfile::tempdir().map_err(|e| LlmError::Unavailable(format!("cannot create a work dir: {e}")))?;
        let config_path = dir.path().join("mcp.json");
        std::fs::write(&config_path, server.mcp_config().to_string())
            .map_err(|e| LlmError::Unavailable(format!("cannot write the MCP config: {e}")))?;

        let system_text = system.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n\n");
        let prompt = user
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text.as_str()),
                ContentPart::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let args = self.args(&system_text, &config_path.to_string_lossy(), tools);
        let mut child = self.spawn(args, dir.path())?;
        feed_prompt(&mut child, prompt)?;
        let stdout = read_all(child.stdout.take());
        let stderr = read_all(child.stderr.take());

        let (ended, turns) = self.pump(&mut child, &mut calls, handler, limits).await;
        if !matches!(ended, Ended::Exited) {
            let _ = child.kill().await;
        }
        let (stdout, stderr) = (collect(stdout).await?, collect(stderr).await?);
        drop(server);

        let result = parse_result(&stdout);
        let ended = if handler.finished() { Ended::Finished } else { ended };
        match ended {
            Ended::Finished => Ok(result.outcome(turns, LoopStop::Finished)),
            Ended::MaxTurns => Ok(result.outcome(turns, LoopStop::MaxTurns)),
            Ended::Timeout => {
                Err(LlmError::Unavailable(format!("claude did not finish within {:?}", self.config.timeout)))
            }
            Ended::Exited => result.into_outcome(turns, &stderr),
        }
    }
}

impl CliToolLoop {
    async fn pump<H: ToolHandler>(
        &self,
        child: &mut Child,
        calls: &mut mpsc::Receiver<ToolCall>,
        handler: &H,
        limits: LoopLimits,
    ) -> (Ended, u32) {
        let mut deadline = pin!(tokio::time::sleep(self.config.timeout));
        let mut in_flight = FuturesUnordered::new();
        let mut grace_started = false;
        let mut turns = 0;
        loop {
            if handler.finished() && !grace_started {
                grace_started = true;
                deadline.as_mut().reset(Instant::now() + self.finish_grace);
            }
            tokio::select! {
                Some(call) = calls.recv() => {
                    turns += 1;
                    if turns > limits.max_turns {
                        let _ = call.reply.send(crate::llm::ToolOutput::error("turn limit reached"));
                        return (Ended::MaxTurns, turns - 1);
                    }
                    in_flight.push(async move {
                        let output = handler.call(&call.name, call.input).await;
                        let _ = call.reply.send(output);
                    });
                }
                Some(()) = in_flight.next(), if !in_flight.is_empty() => {}
                _ = child.wait() => return (Ended::Exited, turns),
                () = &mut deadline => {
                    let ended = if handler.finished() { Ended::Finished } else { Ended::Timeout };
                    return (ended, turns);
                }
            }
        }
    }
}

fn feed_prompt(child: &mut Child, prompt: String) -> Result<(), LlmError> {
    use tokio::io::AsyncWriteExt;
    let mut stdin = child.stdin.take().ok_or_else(|| LlmError::Unavailable("claude stdin unavailable".into()))?;
    tokio::spawn(async move {
        let _ = stdin.write_all(prompt.as_bytes()).await;
    });
    Ok(())
}

async fn collect(reader: JoinHandle<Result<String, LlmError>>) -> Result<String, LlmError> {
    match tokio::time::timeout(OUTPUT_DRAIN, reader).await {
        Ok(joined) => joined.unwrap_or_else(|_| Ok(String::new())),
        Err(_) => Ok(String::new()),
    }
}

fn read_all(stream: Option<impl AsyncRead + Unpin + Send + 'static>) -> JoinHandle<Result<String, LlmError>> {
    tokio::spawn(async move {
        let Some(mut stream) = stream else { return Ok(String::new()) };
        match read_capped(&mut stream, MAX_OUTPUT_BYTES).await {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) if exceeded_cap(&error) => {
                let _ = tokio::io::copy(&mut stream, &mut tokio::io::sink()).await;
                Err(LlmError::Unavailable(format!("claude output exceeded {MAX_OUTPUT_BYTES} bytes")))
            }
            Err(_) => Ok(String::new()),
        }
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CliResult {
    is_error: bool,
    #[serde(rename = "result")]
    text: String,
    api_error_status: Option<u16>,
    #[serde(skip)]
    parsed: bool,
    #[serde(skip)]
    usage: Usage,
    #[serde(skip)]
    cost_usd: f64,
}

fn parse_result(stdout: &str) -> CliResult {
    match serde_json::from_str::<Value>(stdout.trim()).and_then(serde_json::from_value::<CliResult>) {
        Ok(result) => {
            let (usage, cost_usd) = envelope_consumption(stdout);
            CliResult { parsed: true, usage, cost_usd, ..result }
        }
        Err(_) => CliResult::default(),
    }
}

impl CliResult {
    fn into_outcome(self, turns: u32, stderr: &str) -> Result<LoopOutcome, LlmError> {
        if !self.parsed {
            return Err(crate::llm::classify_cli_failure(None, stderr));
        }
        if self.is_error {
            return Err(crate::llm::classify_cli_failure(self.api_error_status, &self.text));
        }
        Ok(self.outcome(turns, LoopStop::NoMoreToolCalls))
    }

    fn outcome(self, turns: u32, stop: LoopStop) -> LoopOutcome {
        LoopOutcome { turns, stop, final_text: self.text, usage: self.usage, cost_usd: self.cost_usd }
    }
}

#[cfg(test)]
#[path = "cli_loop_tests.rs"]
mod tests;
