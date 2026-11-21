use std::path::PathBuf;
use std::time::Duration;

use crate::llm::LlmError;

const VALID_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, PartialEq)]
pub struct CliConfig {
    pub claude_bin: PathBuf,
    pub model: String,
    pub effort: String,
    pub timeout: Duration,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            claude_bin: PathBuf::from("claude"),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            timeout: Duration::from_secs(600),
        }
    }
}

impl CliConfig {
    pub(crate) fn validate(&self) -> Result<(), LlmError> {
        let invalid = |what: String| Err(LlmError::Rejected(format!("invalid Claude CLI settings: {what}")));
        if self.claude_bin.as_os_str().is_empty() {
            return invalid("claude_bin is empty".into());
        }
        if self.model.trim().is_empty() {
            return invalid("model is empty".into());
        }
        if self.model.starts_with('-') {
            return invalid(format!("model must not start with '-', got {:?}", self.model));
        }
        if !VALID_EFFORTS.contains(&self.effort.as_str()) {
            return invalid(format!("effort must be one of {VALID_EFFORTS:?}, got {:?}", self.effort));
        }
        if self.timeout.is_zero() {
            return invalid("timeout must be positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        assert_eq!(CliConfig::default().validate(), Ok(()));
    }

    #[test]
    fn rejects_each_invalid_field() {
        let base = CliConfig::default;
        let reason = |c: CliConfig| match c.validate() {
            Err(LlmError::Rejected(r)) => r,
            other => panic!("expected Rejected, got {other:?}"),
        };
        assert!(reason(CliConfig { claude_bin: PathBuf::new(), ..base() }).contains("claude_bin"));
        assert!(reason(CliConfig { model: " ".into(), ..base() }).contains("model"));
        assert!(reason(CliConfig { model: "--dangerous".into(), ..base() }).contains("model"));
        assert!(reason(CliConfig { effort: "huge".into(), ..base() }).contains("effort"));
        assert!(reason(CliConfig { timeout: Duration::ZERO, ..base() }).contains("timeout"));
    }
}
