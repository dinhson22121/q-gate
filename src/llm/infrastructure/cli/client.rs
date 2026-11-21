use std::path::Path;
use std::process::Stdio;

use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::config::CliConfig;
use super::envelope::{Answer, Envelope, classify_failure};
use crate::llm::{LlmError, Structured, StructuredModel};

const CHARS_PER_TOKEN: usize = 4;
const PROBE_PROMPT: &str = "ok";
const PROBE_EFFORT: &str = "low";
const SPAWN_RETRIES: u32 = 20;
const SPAWN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

pub fn estimate_input_tokens(system: &str, user_prompt: &str, schema: &Value) -> u64 {
    tokens_for_chars(system.chars().count() + user_prompt.chars().count() + schema.to_string().chars().count())
}

pub fn tokens_for_chars(chars: usize) -> u64 {
    chars.div_ceil(CHARS_PER_TOKEN) as u64
}

#[derive(Debug, Clone)]
pub struct CliClient {
    config: CliConfig,
}

impl CliClient {
    pub fn new(config: CliConfig) -> Result<Self, LlmError> {
        config.validate()?;
        Ok(Self { config })
    }

    pub async fn structured<T: DeserializeOwned>(
        &self,
        system: &str,
        user_prompt: &str,
        schema: &Value,
    ) -> Result<Structured<T>, LlmError> {
        self.structured_in(system, user_prompt, schema, None).await
    }

    pub async fn structured_with_files<T: DeserializeOwned>(
        &self,
        system: &str,
        user_prompt: &str,
        schema: &Value,
        share_dir: &Path,
    ) -> Result<Structured<T>, LlmError> {
        self.structured_in(system, user_prompt, schema, Some(share_dir)).await
    }

    async fn structured_in<T: DeserializeOwned>(
        &self,
        system: &str,
        user_prompt: &str,
        schema: &Value,
        share_dir: Option<&Path>,
    ) -> Result<Structured<T>, LlmError> {
        let first = self.attempt::<T>(system, user_prompt, schema, share_dir).await;
        let Err(LlmError::InvalidResponse(reason)) = &first else {
            return first;
        };
        let retry_prompt = format!(
            "{user_prompt}\n\nYour previous answer was rejected ({reason}). Answer again, strictly matching the schema."
        );
        self.attempt(system, &retry_prompt, schema, share_dir).await
    }

    pub async fn preflight(&self) -> Result<(), LlmError> {
        let mut args = self.base_args();
        set_effort(&mut args, PROBE_EFFORT);
        self.run(args, PROBE_PROMPT, None).await.map(|_| ())
    }

    async fn attempt<T: DeserializeOwned>(
        &self,
        system: &str,
        user_prompt: &str,
        schema: &Value,
        share_dir: Option<&Path>,
    ) -> Result<Structured<T>, LlmError> {
        let mut args = self.base_args();
        if let Some(dir) = share_dir {
            enable_read(&mut args, dir);
        }
        args.extend(["--append-system-prompt".into(), system.into(), "--json-schema".into(), schema.to_string()]);
        let Answer { payload, usage, cost_usd } = self.run(args, user_prompt, share_dir).await?;
        let value = match payload {
            Value::String(text) => serde_json::from_str(&text),
            other => serde_json::from_value(other),
        }
        .map_err(|e| LlmError::InvalidResponse(format!("answer does not match the schema: {e}")))?;
        Ok(Structured { value, usage, cost_usd })
    }

    fn base_args(&self) -> Vec<String> {
        let args = [
            "-p",
            "--output-format",
            "json",
            "--model",
            &self.config.model,
            "--effort",
            &self.config.effort,
            "--tools",
            "",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
        ];
        args.map(String::from).to_vec()
    }

    async fn run(&self, args: Vec<String>, prompt: &str, cwd: Option<&Path>) -> Result<Answer, LlmError> {
        let mut command = Command::new(&self.config.claude_bin);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = spawn_with_retry(&mut command)
            .await
            .map_err(|e| LlmError::Unavailable(format!("cannot start {}: {e}", self.config.claude_bin.display())))?;
        let mut stdin = child.stdin.take().ok_or_else(|| LlmError::Unavailable("claude stdin unavailable".into()))?;
        let prompt = prompt.to_owned();
        let feed = tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
        });
        let output = tokio::time::timeout(self.config.timeout, child.wait_with_output())
            .await
            .map_err(|_| LlmError::Unavailable(format!("claude did not finish within {:?}", self.config.timeout)))?
            .map_err(|e| LlmError::Unavailable(format!("claude failed: {e}")))?;
        let _ = feed.await;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        match Envelope::parse(&stdout) {
            Some(envelope) => envelope.into_answer(),
            None => Err(classify_failure(None, &format!("{} {}", stderr.trim(), stdout.trim()))),
        }
    }
}

async fn spawn_with_retry(command: &mut Command) -> std::io::Result<tokio::process::Child> {
    let mut attempts_left = SPAWN_RETRIES;
    loop {
        match command.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts_left > 0 => {
                attempts_left -= 1;
                tokio::time::sleep(SPAWN_RETRY_DELAY).await;
            }
            other => return other,
        }
    }
}

fn enable_read(args: &mut Vec<String>, dir: &Path) {
    if let Some(at) = args.iter().position(|a| a == "--tools") {
        args[at + 1] = "Read".into();
    }
    args.extend(["--add-dir".into(), dir.display().to_string(), "--allowedTools".into(), "Read".into()]);
}

impl StructuredModel for CliClient {
    fn structured_json<'a>(
        &'a self,
        system: &'a str,
        user_prompt: &'a str,
        schema: &'a Value,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
        Box::pin(self.structured(system, user_prompt, schema))
    }

    fn structured_json_with_files<'a>(
        &'a self,
        system: &'a str,
        user_prompt: &'a str,
        schema: &'a Value,
        share_dir: &'a Path,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
        Box::pin(self.structured_with_files(system, user_prompt, schema, share_dir))
    }

    fn preflight(&self) -> BoxFuture<'_, Result<(), LlmError>> {
        Box::pin(CliClient::preflight(self))
    }
}

fn set_effort(args: &mut [String], effort: &str) {
    if let Some(at) = args.iter().position(|a| a == "--effort") {
        args[at + 1] = effort.to_string();
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
