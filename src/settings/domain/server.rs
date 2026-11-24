use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::agents::{AgentsConfig, FigmaConfig};
use super::jira::JiraConfig;
use super::legacy::LegacyField;
use super::output::OutputConfig;
use super::validation::{ValidationError, ValidationResult, require_http_url, require_non_empty, require_range};

const DEFAULT_CLAUDE_BIN: &str = "claude";
const DEFAULT_CLI_TIMEOUT_SECS: u64 = 600;
const MAX_CLI_TIMEOUT_SECS: u64 = 6 * 60 * 60;
const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_ALLOWED_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];
const DEFAULT_REPOS_ROOT: &str = "/repos";
const DEFAULT_SCAN_DEPTH: u32 = 3;
const MAX_SCAN_DEPTH: u32 = 10;
const DEFAULT_POLL_INTERVAL_SECS: u64 = 2;
const MAX_POLL_INTERVAL_SECS: u64 = 3600;
const DEFAULT_MAX_CONCURRENT_REVIEWS: usize = 2;
const MAX_CONCURRENT_REVIEWS: usize = 16;
const DEFAULT_REMOTE: &str = "origin";
const LIST_SEPARATOR: char = ',';

pub const ENV_BIND: &str = "QGATE_BIND";
pub const ENV_REPOS_ROOT: &str = "QGATE_REPOS_ROOT";
pub const ENV_REPOS_AUTO_SCAN: &str = "QGATE_REPOS_AUTO_SCAN";
pub const ENV_WEBHOOK_URL: &str = "QGATE_WEBHOOK_URL";
pub const ENV_CLAUDE_BIN: &str = "QGATE_CLAUDE_BIN";
pub const ENV_CLI_TIMEOUT_SECS: &str = "QGATE_CLI_TIMEOUT_SECS";
pub const ENV_JIRA_BASE_URL: &str = "JIRA_BASE_URL";
pub const ENV_JIRA_ENABLED: &str = "QGATE_JIRA_ENABLED";
pub const ENV_OUTPUT_DIR: &str = "QGATE_OUTPUT_DIR";
pub const ENV_FIGMA_API_BASE_URL: &str = "QGATE_FIGMA_API_BASE_URL";
pub const ENV_FIGMA_MCP_URL: &str = "QGATE_FIGMA_MCP_URL";
pub const ENV_RUN_REPO_CHECKS: &str = "QGATE_RUN_REPO_CHECKS";
pub const ENV_WATCH_BASE_BRANCH: &str = "QGATE_WATCH_BASE_BRANCH";
pub const ENV_QUALITY_ENABLED: &str = "QGATE_QUALITY_ENABLED";
pub const ENV_REGRESSION_ENABLED: &str = "QGATE_REGRESSION_ENABLED";
pub const ENV_REDTEAM_ALLOWED_TARGETS: &str = "QGATE_REDTEAM_ALLOWED_TARGETS";
pub const ENV_REDTEAM_ISOLATED_TARGETS: &str = "QGATE_REDTEAM_ISOLATED_TARGETS";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardConfig {
    pub bind: String,
    pub allowed_hosts: Vec<String>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self { bind: DEFAULT_BIND.into(), allowed_hosts: DEFAULT_ALLOWED_HOSTS.iter().map(|h| h.to_string()).collect() }
    }
}

impl DashboardConfig {
    pub fn bind_addr(&self) -> Result<SocketAddr, ValidationError> {
        self.bind.parse().map_err(|_| {
            ValidationError::new("dashboard.bind", format!("must be an IP:port address, got '{}'", self.bind))
        })
    }

    fn validate(&self) -> ValidationResult {
        self.bind_addr()?;
        if self.allowed_hosts.is_empty() {
            return Err(ValidationError::new("dashboard.allowed_hosts", "must list at least one host"));
        }
        if self.allowed_hosts.iter().any(|h| h.trim().is_empty() || h.contains(char::is_whitespace)) {
            return Err(ValidationError::new(
                "dashboard.allowed_hosts",
                "hosts must be non-empty and contain no spaces",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReposConfig {
    pub roots: Vec<String>,
    pub scan_depth: u32,
    pub auto_scan: bool,
}

impl Default for ReposConfig {
    fn default() -> Self {
        Self { roots: vec![DEFAULT_REPOS_ROOT.into()], scan_depth: DEFAULT_SCAN_DEPTH, auto_scan: false }
    }
}

impl ReposConfig {
    pub fn scanned_roots(&self) -> &[String] {
        if self.auto_scan { &self.roots } else { &[] }
    }

    fn validate(&self) -> ValidationResult {
        if self.roots.is_empty() {
            return Err(ValidationError::new("repos.roots", "must list at least one directory"));
        }
        if let Some(root) = self.roots.iter().find(|r| !Path::new(r).is_absolute()) {
            return Err(ValidationError::new("repos.roots", format!("must be absolute paths, got '{root}'")));
        }
        require_range("repos.scan_depth", self.scan_depth, 1, MAX_SCAN_DEPTH)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifyConfig {
    pub webhook_url: String,
}

impl NotifyConfig {
    fn validate(&self) -> ValidationResult {
        if self.webhook_url.is_empty() {
            return Ok(());
        }
        require_http_url("notify.webhook_url", &self.webhook_url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerWatchConfig {
    pub poll_interval_secs: u64,
    pub max_concurrent_reviews: usize,
    pub run_repo_checks: bool,
    pub remote: String,
    pub base_branch: String,
}

impl Default for ServerWatchConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            max_concurrent_reviews: DEFAULT_MAX_CONCURRENT_REVIEWS,
            run_repo_checks: true,
            remote: DEFAULT_REMOTE.into(),
            base_branch: String::new(),
        }
    }
}

impl ServerWatchConfig {
    fn validate(&self) -> ValidationResult {
        require_range("watch.poll_interval_secs", self.poll_interval_secs, 1, MAX_POLL_INTERVAL_SECS)?;
        require_range("watch.max_concurrent_reviews", self.max_concurrent_reviews, 1, MAX_CONCURRENT_REVIEWS)?;
        require_ref_token("watch.remote", &self.remote, false)?;
        require_ref_token("watch.base_branch", &self.base_branch, true)
    }
}

pub(super) fn require_ref_token(field: &str, value: &str, allow_empty: bool) -> ValidationResult {
    if value.is_empty() {
        return if allow_empty { Ok(()) } else { Err(ValidationError::new(field, "must not be empty")) };
    }
    if value.starts_with('-') || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ValidationError::new(field, "must not start with '-' or contain whitespace or control characters"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityConfig {
    pub enabled: bool,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegressionConfig {
    pub enabled: bool,
}

impl Default for RegressionConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerLlmConfig {
    pub claude_bin: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing, rename = "base_url")]
    legacy_base_url: LegacyField,
}

impl Default for ServerLlmConfig {
    fn default() -> Self {
        Self {
            claude_bin: DEFAULT_CLAUDE_BIN.into(),
            timeout_secs: DEFAULT_CLI_TIMEOUT_SECS,
            legacy_base_url: LegacyField,
        }
    }
}

impl ServerLlmConfig {
    fn validate(&self) -> ValidationResult {
        require_non_empty("llm.claude_bin", &self.claude_bin)?;
        require_range("llm.timeout_secs", self.timeout_secs, 1, MAX_CLI_TIMEOUT_SECS)
    }
}

const DEFAULT_REDTEAM_ALLOWED_TARGETS: [&str; 3] = ["127.0.0.1", "localhost", "::1"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedteamConfig {
    pub allowed_targets: Vec<String>,
    pub isolated_targets: Vec<String>,
}

impl Default for RedteamConfig {
    fn default() -> Self {
        Self {
            allowed_targets: DEFAULT_REDTEAM_ALLOWED_TARGETS.iter().map(|h| h.to_string()).collect(),
            isolated_targets: Vec::new(),
        }
    }
}

impl RedteamConfig {
    fn validate(&self) -> ValidationResult {
        if self.allowed_targets.iter().any(|h| h.trim().is_empty() || h.contains(char::is_whitespace)) {
            return Err(ValidationError::new(
                "redteam.allowed_targets",
                "hosts must be non-empty and contain no spaces",
            ));
        }
        if self.isolated_targets.iter().any(|h| h.trim().is_empty() || h.contains(char::is_whitespace)) {
            return Err(ValidationError::new(
                "redteam.isolated_targets",
                "hosts must be non-empty and contain no spaces",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub dashboard: DashboardConfig,
    pub repos: ReposConfig,
    pub notify: NotifyConfig,
    pub watch: ServerWatchConfig,
    pub quality: QualityConfig,
    pub regression: RegressionConfig,
    pub llm: ServerLlmConfig,
    pub jira: JiraConfig,
    pub agents: AgentsConfig,
    pub figma: FigmaConfig,
    pub output: OutputConfig,
    pub redteam: RedteamConfig,
}

impl ServerConfig {
    pub fn validate(&self) -> ValidationResult {
        self.dashboard.validate()?;
        self.repos.validate()?;
        self.notify.validate()?;
        self.watch.validate()?;
        self.llm.validate()?;
        self.jira.validate("jira")?;
        self.agents.validate()?;
        self.figma.validate()?;
        self.output.validate()?;
        self.redteam.validate()
    }

    pub fn with_env_overrides(mut self, env: impl Fn(&str) -> Option<String>) -> Self {
        for (entry, value) in active_env_overrides(&env) {
            (entry.apply)(&mut self, &value);
        }
        self
    }
}

#[derive(Clone, Copy)]
pub struct EnvOverride {
    pub env_var: &'static str,
    pub field: &'static str,
    apply: fn(&mut ServerConfig, &str),
}

impl std::fmt::Debug for EnvOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvOverride").field("env_var", &self.env_var).field("field", &self.field).finish()
    }
}

pub const ENV_OVERRIDES: [EnvOverride; 17] = [
    EnvOverride { env_var: ENV_BIND, field: "dashboard.bind", apply: |c, v| c.dashboard.bind = v.into() },
    EnvOverride { env_var: ENV_REPOS_ROOT, field: "repos.roots", apply: |c, v| c.repos.roots = split_list(v) },
    EnvOverride {
        env_var: ENV_REPOS_AUTO_SCAN,
        field: "repos.auto_scan",
        apply: |c, v| c.repos.auto_scan = parse_flag(v),
    },
    EnvOverride {
        env_var: ENV_WEBHOOK_URL,
        field: "notify.webhook_url",
        apply: |c, v| c.notify.webhook_url = v.into(),
    },
    EnvOverride { env_var: ENV_CLAUDE_BIN, field: "llm.claude_bin", apply: |c, v| c.llm.claude_bin = v.into() },
    EnvOverride {
        env_var: ENV_CLI_TIMEOUT_SECS,
        field: "llm.timeout_secs",
        apply: |c, v| c.llm.timeout_secs = v.trim().parse().unwrap_or(c.llm.timeout_secs),
    },
    EnvOverride { env_var: ENV_JIRA_BASE_URL, field: "jira.base_url", apply: |c, v| c.jira.base_url = v.into() },
    EnvOverride { env_var: ENV_JIRA_ENABLED, field: "jira.enabled", apply: |c, v| c.jira.enabled = parse_flag(v) },
    EnvOverride { env_var: ENV_OUTPUT_DIR, field: "output.dir", apply: |c, v| c.output.dir = v.into() },
    EnvOverride {
        env_var: ENV_FIGMA_API_BASE_URL,
        field: "figma.api_base_url",
        apply: |c, v| c.figma.api_base_url = v.into(),
    },
    EnvOverride { env_var: ENV_FIGMA_MCP_URL, field: "figma.mcp_url", apply: |c, v| c.figma.mcp_url = v.into() },
    EnvOverride {
        env_var: ENV_RUN_REPO_CHECKS,
        field: "watch.run_repo_checks",
        apply: |c, v| c.watch.run_repo_checks = parse_flag(v),
    },
    EnvOverride {
        env_var: ENV_WATCH_BASE_BRANCH,
        field: "watch.base_branch",
        apply: |c, v| c.watch.base_branch = v.into(),
    },
    EnvOverride {
        env_var: ENV_QUALITY_ENABLED,
        field: "quality.enabled",
        apply: |c, v| c.quality.enabled = parse_flag(v),
    },
    EnvOverride {
        env_var: ENV_REGRESSION_ENABLED,
        field: "regression.enabled",
        apply: |c, v| c.regression.enabled = parse_flag(v),
    },
    EnvOverride {
        env_var: ENV_REDTEAM_ALLOWED_TARGETS,
        field: "redteam.allowed_targets",
        apply: |c, v| c.redteam.allowed_targets = split_list(v),
    },
    EnvOverride {
        env_var: ENV_REDTEAM_ISOLATED_TARGETS,
        field: "redteam.isolated_targets",
        apply: |c, v| c.redteam.isolated_targets = split_list(v),
    },
];

pub const ADMIN_ONLY_FIELDS: [&str; 9] = [
    "repos.auto_scan",
    "llm.claude_bin",
    "llm.timeout_secs",
    "jira.base_url",
    "figma.api_base_url",
    "figma.mcp_url",
    "watch.base_branch",
    "redteam.allowed_targets",
    "redteam.isolated_targets",
];
pub const WEBHOOK_URL_FIELD: &str = "notify.webhook_url";
pub const RUN_REPO_CHECKS_FIELD: &str = "watch.run_repo_checks";

pub fn admin_env_var(field: &str) -> Option<&'static str> {
    ENV_OVERRIDES.iter().find(|entry| entry.field == field).map(|entry| entry.env_var)
}

pub const RESTART_REQUIRED_FIELDS: [&str; 9] = [
    "dashboard.bind",
    "dashboard.allowed_hosts",
    "repos.roots",
    "repos.scan_depth",
    "watch.poll_interval_secs",
    "watch.max_concurrent_reviews",
    "watch.remote",
    "watch.base_branch",
    "output.dir",
];

pub fn active_env_overrides(env: &impl Fn(&str) -> Option<String>) -> Vec<(EnvOverride, String)> {
    ENV_OVERRIDES
        .iter()
        .filter_map(|entry| {
            let value = env(entry.env_var).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())?;
            Some((*entry, value))
        })
        .collect()
}

fn parse_flag(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn split_list(value: &str) -> Vec<String> {
    value.split(LIST_SEPARATOR).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn defaults_are_valid() {
        let config = ServerConfig::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.dashboard.bind_addr().map(|a| a.port()), Ok(8080));
        assert_eq!(config.repos.roots, vec!["/repos".to_string()]);
        assert_eq!(config.watch.max_concurrent_reviews, 2);
        assert_eq!(config.watch.remote, "origin");
        assert_eq!(config.watch.base_branch, "");
        assert_eq!((config.llm.claude_bin.as_str(), config.llm.timeout_secs), ("claude", 600));
    }

    #[test]
    fn settings_saved_with_the_removed_base_url_still_load() {
        let config: ServerConfig = toml::from_str("[llm]\nbase_url = \"https://api.anthropic.com\"\n").unwrap();
        assert_eq!(config, ServerConfig::default());
        let written: toml::Value = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert!(written["llm"].get("base_url").is_none());
        let json: ServerConfig = serde_json::from_str(r#"{"llm":{"base_url":"http://127.0.0.1:4000"}}"#).unwrap();
        assert_eq!(json.llm.claude_bin, "claude");
    }

    #[test]
    fn an_unparsable_timeout_env_keeps_the_configured_value() {
        let config = ServerConfig::default().with_env_overrides(env(&[(ENV_CLI_TIMEOUT_SECS, "soon")]));
        assert_eq!(config.llm.timeout_secs, 600);
    }

    #[test]
    fn env_overrides_replace_common_fields() {
        let config = ServerConfig::default().with_env_overrides(env(&[
            (ENV_BIND, "0.0.0.0:9000"),
            (ENV_REPOS_ROOT, "/repos/a, /repos/b,,"),
            (ENV_WEBHOOK_URL, "http://ntfy/qgate"),
            (ENV_CLAUDE_BIN, "/opt/claude/bin/claude"),
            (ENV_CLI_TIMEOUT_SECS, "900"),
            (ENV_JIRA_BASE_URL, "https://acme.atlassian.net"),
            (ENV_OUTPUT_DIR, "/data/reviews"),
        ]));
        assert_eq!(config.dashboard.bind, "0.0.0.0:9000");
        assert_eq!(config.repos.roots, vec!["/repos/a".to_string(), "/repos/b".to_string()]);
        assert_eq!(config.notify.webhook_url, "http://ntfy/qgate");
        assert_eq!((config.llm.claude_bin.as_str(), config.llm.timeout_secs), ("/opt/claude/bin/claude", 900));
        assert_eq!(config.jira.base_url, "https://acme.atlassian.net");
        assert_eq!(config.output.dir, "/data/reviews");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn quality_and_regression_default_on_and_env_overrides_turn_them_off() {
        let defaults = ServerConfig::default();
        assert!(defaults.quality.enabled && defaults.regression.enabled);
        let off = ServerConfig::default()
            .with_env_overrides(env(&[(ENV_QUALITY_ENABLED, "false"), (ENV_REGRESSION_ENABLED, "0")]));
        assert!(!off.quality.enabled && !off.regression.enabled);
        let on = off.with_env_overrides(env(&[(ENV_QUALITY_ENABLED, "true")]));
        assert!(on.quality.enabled && !on.regression.enabled);
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let config = ServerConfig::default().with_env_overrides(env(&[(ENV_BIND, "  ")]));
        assert_eq!(config, ServerConfig::default());
    }

    #[test]
    fn invalid_values_are_rejected_with_field_path() {
        let base = ServerConfig::default();
        let mut cases = Vec::new();
        let mut c = base.clone();
        c.dashboard.bind = "localhost".into();
        cases.push((c, "dashboard.bind"));
        let mut c = base.clone();
        c.dashboard.allowed_hosts = vec![];
        cases.push((c, "dashboard.allowed_hosts"));
        let mut c = base.clone();
        c.dashboard.allowed_hosts = vec!["a b".into()];
        cases.push((c, "dashboard.allowed_hosts"));
        let mut c = base.clone();
        c.repos.roots = vec![];
        cases.push((c, "repos.roots"));
        let mut c = base.clone();
        c.repos.roots = vec!["relative/dir".into()];
        cases.push((c, "repos.roots"));
        let mut c = base.clone();
        c.repos.scan_depth = 0;
        cases.push((c, "repos.scan_depth"));
        let mut c = base.clone();
        c.notify.webhook_url = "not-a-url".into();
        cases.push((c, "notify.webhook_url"));
        let mut c = base.clone();
        c.watch.poll_interval_secs = 0;
        cases.push((c, "watch.poll_interval_secs"));
        let mut c = base.clone();
        c.watch.max_concurrent_reviews = 17;
        cases.push((c, "watch.max_concurrent_reviews"));
        let mut c = base.clone();
        c.watch.remote = String::new();
        cases.push((c, "watch.remote"));
        let mut c = base.clone();
        c.watch.base_branch = "bad branch".into();
        cases.push((c, "watch.base_branch"));
        let mut c = base.clone();
        c.llm.claude_bin = " ".into();
        cases.push((c, "llm.claude_bin"));
        let mut c = base.clone();
        c.llm.timeout_secs = 0;
        cases.push((c, "llm.timeout_secs"));
        let mut c = base.clone();
        c.jira.base_url = "http://jira.example.com".into();
        cases.push((c, "jira.base_url"));
        let mut c = base;
        c.output.path_template = "{repo}/{nope}.md".into();
        cases.push((c, "output.path_template"));
        for (config, field) in cases {
            assert_eq!(config.validate().unwrap_err().field, field);
        }
    }

    #[test]
    fn jira_can_be_enabled_from_env() {
        let enabled = ServerConfig::default().with_env_overrides(env(&[(ENV_JIRA_ENABLED, "TRUE")]));
        assert!(enabled.jira.enabled);
        let disabled = enabled.with_env_overrides(env(&[(ENV_JIRA_ENABLED, "0")]));
        assert!(!disabled.jira.enabled);
    }

    #[test]
    fn active_overrides_name_their_field_and_skip_blank_values() {
        let active = active_env_overrides(&env(&[(ENV_JIRA_BASE_URL, " https://a.atlassian.net "), (ENV_BIND, " ")]));
        assert_eq!(active.len(), 1);
        assert_eq!(
            (active[0].0.field, active[0].0.env_var, active[0].1.as_str()),
            ("jira.base_url", ENV_JIRA_BASE_URL, "https://a.atlassian.net")
        );
        assert!(format!("{:?}", active[0].0).contains("jira.base_url"));
        assert_eq!(ENV_OVERRIDES.len(), 17);
    }

    #[test]
    fn admin_only_fields_are_settable_from_env() {
        let config = ServerConfig::default().with_env_overrides(env(&[
            (ENV_FIGMA_API_BASE_URL, "https://figma.proxy.example"),
            (ENV_FIGMA_MCP_URL, "https://mcp.proxy.example/mcp"),
            (ENV_RUN_REPO_CHECKS, "off"),
            (ENV_WATCH_BASE_BRANCH, "develop"),
            (ENV_REPOS_AUTO_SCAN, "true"),
        ]));
        assert_eq!(config.figma.api_base_url, "https://figma.proxy.example");
        assert_eq!(config.figma.mcp_url, "https://mcp.proxy.example/mcp");
        assert!(!config.watch.run_repo_checks);
        assert_eq!(config.watch.base_branch, "develop");
        assert!(config.repos.auto_scan);
        assert!(config.validate().is_ok());
        for field in ADMIN_ONLY_FIELDS.iter().chain([&WEBHOOK_URL_FIELD, &RUN_REPO_CHECKS_FIELD]) {
            assert!(admin_env_var(field).is_some(), "{field} needs an env override");
        }
        assert_eq!(admin_env_var("agents.model"), None);
    }

    #[test]
    fn scanning_is_off_by_default_so_no_root_is_scanned() {
        let config = ReposConfig::default();
        assert!(!config.auto_scan);
        assert!(config.scanned_roots().is_empty());
        let scanning = ReposConfig { auto_scan: true, ..config };
        assert_eq!(scanning.scanned_roots(), [DEFAULT_REPOS_ROOT.to_string()]);
    }
}
