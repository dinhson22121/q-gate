use serde::{Deserialize, Deserializer, Serialize};

use super::validation::{ValidationError, ValidationResult, require_secure_url};

const DEFAULT_BASE_URL: &str = "https://yourcompany.atlassian.net";
const CUSTOM_FIELD_PREFIX: &str = "customfield_";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JiraProvider {
    #[default]
    McpOauth,
}

impl<'de> Deserialize<'de> for JiraProvider {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?;
        Ok(Self::McpOauth)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JiraConfig {
    pub enabled: bool,
    pub base_url: String,
    pub provider: JiraProvider,
    pub ac_field: String,
    pub key_pattern_projects: Vec<String>,
}

impl Default for JiraConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: DEFAULT_BASE_URL.into(),
            provider: JiraProvider::default(),
            ac_field: String::new(),
            key_pattern_projects: Vec::new(),
        }
    }
}

impl JiraConfig {
    pub fn validate(&self, prefix: &str) -> ValidationResult {
        self.validate_format(prefix)
    }

    pub fn validate_format(&self, prefix: &str) -> ValidationResult {
        require_secure_url(&format!("{prefix}.base_url"), &self.base_url)?;
        validate_ac_field(&format!("{prefix}.ac_field"), &self.ac_field)?;
        for key in &self.key_pattern_projects {
            validate_project_key(&format!("{prefix}.key_pattern_projects"), key)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JiraOverride {
    pub enabled: Option<bool>,
    pub ac_field: Option<String>,
    pub key_pattern_projects: Option<Vec<String>>,
}

impl JiraOverride {
    pub fn apply_to(&self, defaults: &JiraConfig) -> JiraConfig {
        JiraConfig {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            base_url: defaults.base_url.clone(),
            provider: defaults.provider,
            ac_field: self.ac_field.clone().unwrap_or_else(|| defaults.ac_field.clone()),
            key_pattern_projects: self
                .key_pattern_projects
                .clone()
                .unwrap_or_else(|| defaults.key_pattern_projects.clone()),
        }
    }

    pub fn validate(&self, prefix: &str) -> ValidationResult {
        self.apply_to(&JiraConfig::default()).validate_format(prefix)
    }
}

pub(crate) fn require_command(field: &str, command: &[String]) -> ValidationResult {
    match command.first() {
        Some(program) if !program.trim().is_empty() => Ok(()),
        _ => Err(ValidationError::new(field, "must start with a program name")),
    }
}

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
    fn default_provider_is_mcp_oauth_and_enabled_oauth_needs_nothing_else() {
        assert_eq!(JiraProvider::default(), JiraProvider::McpOauth);
        assert_eq!(JiraConfig::default().provider, JiraProvider::McpOauth);
        let config = JiraConfig { enabled: true, ..JiraConfig::default() };
        assert!(config.validate("jira").is_ok());
    }

    #[test]
    fn legacy_and_unknown_providers_deserialize_to_mcp_oauth() {
        for raw in ["\"rest\"", "\"mcp\"", "\"nonsense\"", "\"\""] {
            assert_eq!(serde_json::from_str::<JiraProvider>(raw).unwrap(), JiraProvider::McpOauth, "{raw}");
        }
    }

    #[test]
    fn mcp_oauth_provider_round_trips() {
        let raw = "\"mcp_oauth\"";
        assert_eq!(serde_json::to_string(&JiraProvider::McpOauth).unwrap(), raw);
        assert_eq!(serde_json::from_str::<JiraProvider>(raw).unwrap(), JiraProvider::McpOauth);
    }

    #[test]
    fn legacy_rest_provider_in_a_config_section_still_parses() {
        let config: JiraConfig = serde_json::from_str(r#"{"provider":"rest"}"#).unwrap();
        assert_eq!(config.provider, JiraProvider::McpOauth);
    }

    #[test]
    fn rejects_malformed_values() {
        let base = JiraConfig::default();
        let cases = [
            (JiraConfig { base_url: "http://jira.example.com".into(), ..base.clone() }, "jira.base_url"),
            (JiraConfig { ac_field: "customfield_".into(), ..base.clone() }, "jira.ac_field"),
            (JiraConfig { ac_field: "summary".into(), ..base.clone() }, "jira.ac_field"),
            (JiraConfig { key_pattern_projects: vec!["proj".into()], ..base }, "jira.key_pattern_projects"),
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
        let defaults =
            JiraConfig { enabled: true, base_url: "https://acme.atlassian.net".into(), ..JiraConfig::default() };
        let repo = JiraOverride { ac_field: Some("customfield_1".into()), ..JiraOverride::default() };
        let merged = repo.apply_to(&defaults);
        assert_eq!(merged.base_url, "https://acme.atlassian.net");
        assert_eq!(merged.ac_field, "customfield_1");
        assert!(merged.enabled);

        let disabled = JiraOverride { enabled: Some(false), ..JiraOverride::default() }.apply_to(&defaults);
        assert!(!disabled.enabled);
    }

    #[test]
    fn override_validation_checks_format_but_not_completeness() {
        let enabled = JiraOverride { enabled: Some(true), ..JiraOverride::default() };
        assert!(enabled.validate("jira").is_ok());
        let bad = JiraOverride { ac_field: Some("x".into()), ..JiraOverride::default() };
        assert_eq!(bad.validate("jira").unwrap_err().field, "jira.ac_field");
    }
}
