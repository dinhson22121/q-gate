//! Jira connection settings, shared by the server config (defaults) and repository configs (overrides).

use serde::{Deserialize, Serialize};

use super::validation::{ValidationError, ValidationResult, require_env_name, require_non_empty, require_secure_url};

const DEFAULT_BASE_URL: &str = "https://yourcompany.atlassian.net";
const DEFAULT_MCP_TOOL: &str = "jira_get_issue";
const CUSTOM_FIELD_PREFIX: &str = "customfield_";

/// How acceptance criteria are fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JiraProvider {
    /// Jira Cloud REST API with email + API token.
    #[default]
    Rest,
    /// An MCP server (e.g. `mcp-atlassian`) started as a child process.
    Mcp,
}

/// MCP server used when `provider = "mcp"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JiraMcpConfig {
    /// Program and arguments, started without a shell.
    pub command: Vec<String>,
    /// Tool that returns an issue by key.
    pub tool: String,
    /// Environment variables forwarded to the MCP server (everything else is cleared).
    pub env_passthrough: Vec<String>,
}

impl Default for JiraMcpConfig {
    fn default() -> Self {
        Self {
            command: vec!["uvx".into(), "mcp-atlassian==0.23.1".into()],
            tool: DEFAULT_MCP_TOOL.into(),
            env_passthrough: vec!["JIRA_URL".into(), "JIRA_USERNAME".into(), "JIRA_API_TOKEN".into()],
        }
    }
}

/// Fully resolved Jira settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JiraConfig {
    pub enabled: bool,
    pub base_url: String,
    pub email: String,
    pub provider: JiraProvider,
    /// Optional custom field id holding the acceptance criteria (e.g. `customfield_10050`).
    pub ac_field: String,
    /// Optional allow-list of project keys recognised in branch names / commit messages.
    pub key_pattern_projects: Vec<String>,
    pub mcp: JiraMcpConfig,
}

impl Default for JiraConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: DEFAULT_BASE_URL.into(),
            email: String::new(),
            provider: JiraProvider::Rest,
            ac_field: String::new(),
            key_pattern_projects: Vec::new(),
            mcp: JiraMcpConfig::default(),
        }
    }
}

impl JiraConfig {
    /// Full validation: formats, plus everything the selected provider needs when enabled.
    pub fn validate(&self, prefix: &str) -> ValidationResult {
        self.validate_format(prefix)?;
        if !self.enabled {
            return Ok(());
        }
        match self.provider {
            JiraProvider::Rest => require_non_empty(&format!("{prefix}.email"), &self.email),
            JiraProvider::Mcp => {
                require_command(&format!("{prefix}.mcp.command"), &self.mcp.command)?;
                require_non_empty(&format!("{prefix}.mcp.tool"), &self.mcp.tool)
            }
        }
    }

    /// Checks the shape of each value without requiring the settings to be complete.
    pub fn validate_format(&self, prefix: &str) -> ValidationResult {
        require_secure_url(&format!("{prefix}.base_url"), &self.base_url)?;
        validate_ac_field(&format!("{prefix}.ac_field"), &self.ac_field)?;
        for key in &self.key_pattern_projects {
            validate_project_key(&format!("{prefix}.key_pattern_projects"), key)?;
        }
        for name in &self.mcp.env_passthrough {
            require_env_name(&format!("{prefix}.mcp.env_passthrough"), name)?;
        }
        Ok(())
    }
}

/// Repository-level `[jira]` section: every field is optional and falls back to the server defaults.
///
/// Only harmless settings can be overridden. The endpoint, account, provider and MCP command decide where
/// the Jira API token goes (or which program receives it), so they come from the server config only:
/// a repository's `.qgate.toml` is controlled by whoever can push a commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JiraOverride {
    pub enabled: Option<bool>,
    pub ac_field: Option<String>,
    pub key_pattern_projects: Option<Vec<String>>,
}

impl JiraOverride {
    /// The values set here, falling back to `defaults` for anything left unset.
    pub fn apply_to(&self, defaults: &JiraConfig) -> JiraConfig {
        JiraConfig {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            base_url: defaults.base_url.clone(),
            email: defaults.email.clone(),
            provider: defaults.provider,
            ac_field: self.ac_field.clone().unwrap_or_else(|| defaults.ac_field.clone()),
            key_pattern_projects: self
                .key_pattern_projects
                .clone()
                .unwrap_or_else(|| defaults.key_pattern_projects.clone()),
            mcp: defaults.mcp.clone(),
        }
    }

    /// Checks the format of the values that are set (completeness is checked after merging).
    pub fn validate(&self, prefix: &str) -> ValidationResult {
        self.apply_to(&JiraConfig::default()).validate_format(prefix)
    }
}

/// Requires a non-empty program name.
pub(crate) fn require_command(field: &str, command: &[String]) -> ValidationResult {
    match command.first() {
        Some(program) if !program.trim().is_empty() => Ok(()),
        _ => Err(ValidationError::new(field, "must start with a program name")),
    }
}

/// Field ids end up in API URLs, so only `customfield_<digits>` (or empty) is accepted.
fn validate_ac_field(field: &str, value: &str) -> ValidationResult {
    if value.is_empty() {
        return Ok(());
    }
    let digits = value.strip_prefix(CUSTOM_FIELD_PREFIX).unwrap_or_default();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(ValidationError::new(field, format!("must look like 'customfield_10050', got '{value}'")));
    }
    Ok(())
}

/// Jira project keys: an uppercase letter followed by uppercase letters, digits or `_`.
fn validate_project_key(field: &str, key: &str) -> ValidationResult {
    let mut chars = key.chars();
    let valid_start = chars.next().is_some_and(|c| c.is_ascii_uppercase());
    if !valid_start || !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return Err(ValidationError::new(field, format!("'{key}' is not a valid Jira project key (e.g. 'PROJ')")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_defaults_are_valid() {
        assert!(JiraConfig::default().validate("jira").is_ok());
    }

    #[test]
    fn enabled_rest_requires_email() {
        let config = JiraConfig { enabled: true, ..JiraConfig::default() };
        assert_eq!(config.validate("jira").unwrap_err().field, "jira.email");
        let config = JiraConfig { email: "me@example.com".into(), ..config };
        assert!(config.validate("jira").is_ok());
    }

    #[test]
    fn enabled_mcp_requires_command_and_tool() {
        let mut config = JiraConfig { enabled: true, provider: JiraProvider::Mcp, ..JiraConfig::default() };
        assert!(config.validate("jira").is_ok());
        config.mcp.command = vec![];
        assert_eq!(config.validate("jira").unwrap_err().field, "jira.mcp.command");
        config.mcp.command = vec!["uvx".into()];
        config.mcp.tool = String::new();
        assert_eq!(config.validate("jira").unwrap_err().field, "jira.mcp.tool");
    }

    #[test]
    fn rejects_malformed_values() {
        let base = JiraConfig::default();
        let cases = [
            (JiraConfig { base_url: "http://jira.example.com".into(), ..base.clone() }, "jira.base_url"),
            (JiraConfig { ac_field: "customfield_".into(), ..base.clone() }, "jira.ac_field"),
            (JiraConfig { ac_field: "summary".into(), ..base.clone() }, "jira.ac_field"),
            (JiraConfig { key_pattern_projects: vec!["proj".into()], ..base.clone() }, "jira.key_pattern_projects"),
            (
                JiraConfig {
                    mcp: JiraMcpConfig { env_passthrough: vec!["A B".into()], ..JiraMcpConfig::default() },
                    ..base
                },
                "jira.mcp.env_passthrough",
            ),
        ];
        for (config, field) in cases {
            assert_eq!(config.validate("jira").unwrap_err().field, field);
        }
    }

    #[test]
    fn accepts_well_formed_optional_values() {
        let config = JiraConfig {
            ac_field: "customfield_10050".into(),
            key_pattern_projects: vec!["PROJ".into(), "AB_2".into()],
            ..JiraConfig::default()
        };
        assert!(config.validate("jira").is_ok());
    }

    #[test]
    fn override_falls_back_to_defaults_field_by_field() {
        let defaults = JiraConfig {
            enabled: true,
            base_url: "https://acme.atlassian.net".into(),
            email: "bot@acme.com".into(),
            ..JiraConfig::default()
        };
        let repo = JiraOverride { ac_field: Some("customfield_1".into()), ..JiraOverride::default() };
        let merged = repo.apply_to(&defaults);
        assert_eq!(merged.base_url, "https://acme.atlassian.net");
        assert_eq!(merged.email, "bot@acme.com");
        assert_eq!(merged.ac_field, "customfield_1");
        assert!(merged.enabled);

        let disabled = JiraOverride { enabled: Some(false), ..JiraOverride::default() }.apply_to(&defaults);
        assert!(!disabled.enabled);
    }

    #[test]
    fn override_validation_checks_format_but_not_completeness() {
        let enabled_without_email = JiraOverride { enabled: Some(true), ..JiraOverride::default() };
        assert!(enabled_without_email.validate("jira").is_ok());
        let bad = JiraOverride { ac_field: Some("x".into()), ..JiraOverride::default() };
        assert_eq!(bad.validate("jira").unwrap_err().field, "jira.ac_field");
    }
}
