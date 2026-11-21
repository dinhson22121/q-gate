//! Secrets the application needs and a wrapper that keeps them out of logs.

use std::fmt;

const REDACTED: &str = "***";

/// The secrets qgate knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialName {
    AnthropicApiKey,
    JiraApiToken,
    FigmaToken,
}

impl CredentialName {
    pub const ALL: [Self; 3] = [Self::AnthropicApiKey, Self::JiraApiToken, Self::FigmaToken];

    /// Key used in credential stores.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicApiKey => "anthropic_api_key",
            Self::JiraApiToken => "jira_api_token",
            Self::FigmaToken => "figma_token",
        }
    }

    /// The name for a store key such as `"figma_token"` (the inverse of [`CredentialName::as_str`]).
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|name| name.as_str() == key)
    }

    /// Environment variable that holds the secret directly.
    pub fn env_var(self) -> &'static str {
        match self {
            Self::AnthropicApiKey => "ANTHROPIC_API_KEY",
            Self::JiraApiToken => "JIRA_API_TOKEN",
            Self::FigmaToken => "FIGMA_TOKEN",
        }
    }

    /// Environment variable naming a file that holds the secret (Docker secrets).
    pub fn file_env_var(self) -> &'static str {
        match self {
            Self::AnthropicApiKey => "ANTHROPIC_API_KEY_FILE",
            Self::JiraApiToken => "JIRA_API_TOKEN_FILE",
            Self::FigmaToken => "FIGMA_TOKEN_FILE",
        }
    }
}

impl fmt::Display for CredentialName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A secret value. `Debug` and `Display` never reveal it; use [`Secret::expose`] at the point of use.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({REDACTED})")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_redacted_in_debug_and_display() {
        let secret = Secret::new("sk-ant-very-secret");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(secret.to_string(), "***");
        assert_eq!(format!("{:?}", Some(secret.clone())), "Some(Secret(***))");
        assert_eq!(secret.expose(), "sk-ant-very-secret");
    }

    #[test]
    fn names_map_to_keys_and_env_vars() {
        assert_eq!(CredentialName::AnthropicApiKey.env_var(), "ANTHROPIC_API_KEY");
        assert_eq!(CredentialName::JiraApiToken.env_var(), "JIRA_API_TOKEN");
        assert_eq!(CredentialName::AnthropicApiKey.file_env_var(), "ANTHROPIC_API_KEY_FILE");
        assert_eq!(CredentialName::JiraApiToken.file_env_var(), "JIRA_API_TOKEN_FILE");
        assert_eq!(CredentialName::JiraApiToken.to_string(), "jira_api_token");
        assert_eq!(CredentialName::FigmaToken.env_var(), "FIGMA_TOKEN");
        assert_eq!(CredentialName::ALL.len(), 3);
        assert_eq!(CredentialName::from_key("figma_token"), Some(CredentialName::FigmaToken));
        assert_eq!(CredentialName::from_key("nope"), None);
    }
}
