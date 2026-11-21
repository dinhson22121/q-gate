use std::fmt;

const REDACTED: &str = "***";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialName {
    JiraApiToken,
    FigmaToken,
}

impl CredentialName {
    pub const ALL: [Self; 2] = [Self::JiraApiToken, Self::FigmaToken];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::JiraApiToken => "jira_api_token",
            Self::FigmaToken => "figma_token",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|name| name.as_str() == key)
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Self::JiraApiToken => "JIRA_API_TOKEN",
            Self::FigmaToken => "FIGMA_TOKEN",
        }
    }

    pub fn file_env_var(self) -> &'static str {
        match self {
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
        assert_eq!(CredentialName::JiraApiToken.env_var(), "JIRA_API_TOKEN");
        assert_eq!(CredentialName::JiraApiToken.file_env_var(), "JIRA_API_TOKEN_FILE");
        assert_eq!(CredentialName::JiraApiToken.to_string(), "jira_api_token");
        assert_eq!(CredentialName::FigmaToken.env_var(), "FIGMA_TOKEN");
        assert_eq!(CredentialName::ALL.len(), 2);
        assert_eq!(CredentialName::from_key("figma_token"), Some(CredentialName::FigmaToken));
        assert_eq!(CredentialName::from_key("nope"), None);
    }
}
