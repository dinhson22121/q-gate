//! User-level server configuration (`<config_dir>/qgate.toml`): dashboard, repositories, notifications,
//! watcher, the Anthropic endpoint and default Jira settings. Unlike `.qgate.toml` it is not part of any repository.

use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::agents::{AgentsConfig, FigmaConfig};
use super::jira::JiraConfig;
use super::output::OutputConfig;
use super::validation::{ValidationError, ValidationResult, require_http_url, require_range, require_secure_url};

const DEFAULT_LLM_BASE_URL: &str = "https://api.anthropic.com";
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

/// Environment variables that override the most common server settings.
pub const ENV_BIND: &str = "QGATE_BIND";
pub const ENV_REPOS_ROOT: &str = "QGATE_REPOS_ROOT";
pub const ENV_WEBHOOK_URL: &str = "QGATE_WEBHOOK_URL";
pub const ENV_LLM_BASE_URL: &str = "QGATE_LLM_BASE_URL";
pub const ENV_JIRA_BASE_URL: &str = "JIRA_BASE_URL";
pub const ENV_JIRA_EMAIL: &str = "JIRA_EMAIL";
pub const ENV_JIRA_ENABLED: &str = "QGATE_JIRA_ENABLED";
pub const ENV_OUTPUT_DIR: &str = "QGATE_OUTPUT_DIR";
pub const ENV_JIRA_MCP_COMMAND: &str = "QGATE_JIRA_MCP_COMMAND";
pub const ENV_JIRA_MCP_ENV_PASSTHROUGH: &str = "QGATE_JIRA_MCP_ENV_PASSTHROUGH";
pub const ENV_FIGMA_API_BASE_URL: &str = "QGATE_FIGMA_API_BASE_URL";
pub const ENV_FIGMA_MCP_URL: &str = "QGATE_FIGMA_MCP_URL";
pub const ENV_RUN_REPO_CHECKS: &str = "QGATE_RUN_REPO_CHECKS";
pub const ENV_WATCH_BASE_BRANCH: &str = "QGATE_WATCH_BASE_BRANCH";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardConfig {
    /// Socket address the dashboard listens on.
    pub bind: String,
    /// Accepted `Host` header values (DNS-rebinding protection).
    pub allowed_hosts: Vec<String>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self { bind: DEFAULT_BIND.into(), allowed_hosts: DEFAULT_ALLOWED_HOSTS.iter().map(|h| h.to_string()).collect() }
    }
}

impl DashboardConfig {
    /// The parsed bind address (always valid after [`ServerConfig::validate`]).
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
    /// Absolute directories scanned for git repositories.
    pub roots: Vec<String>,
    /// How many directory levels below each root are searched.
    pub scan_depth: u32,
}

impl Default for ReposConfig {
    fn default() -> Self {
        Self { roots: vec![DEFAULT_REPOS_ROOT.into()], scan_depth: DEFAULT_SCAN_DEPTH }
    }
}

impl ReposConfig {
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
    /// Webhook receiving review results; empty disables it.
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
    /// Run the `[[checks]]` commands declared in each repository's `.qgate.toml`.
    /// They execute repository-controlled commands, so turn this off for repos you do not trust.
    pub run_repo_checks: bool,
    /// Git remote whose tracking refs (`refs/remotes/<remote>/*`) are watched for pushes.
    pub remote: String,
    /// Branch a pushed branch is compared against (merge-base). Empty = auto-resolve
    /// `<remote>/main`, then `<remote>/master`. A push to this branch is never reviewed.
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

/// A git remote or branch name: no whitespace, control characters or leading '-'. `allow_empty`
/// permits "" (used for `watch.base_branch`, where empty means auto-resolve).
fn require_ref_token(field: &str, value: &str, allow_empty: bool) -> ValidationResult {
    if value.is_empty() {
        return if allow_empty { Ok(()) } else { Err(ValidationError::new(field, "must not be empty")) };
    }
    if value.starts_with('-') || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ValidationError::new(field, "must not start with '-' or contain whitespace or control characters"));
    }
    Ok(())
}

/// Anthropic API connection. Only the server config may set it: the endpoint receives the API key and
/// every reviewed diff, so a repository's `.qgate.toml` (attacker-controllable) must not choose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerLlmConfig {
    /// API endpoint; must be https except for a local proxy (localhost / 127.0.0.1).
    pub base_url: String,
}

impl Default for ServerLlmConfig {
    fn default() -> Self {
        Self { base_url: DEFAULT_LLM_BASE_URL.into() }
    }
}

impl ServerLlmConfig {
    fn validate(&self) -> ValidationResult {
        require_secure_url("llm.base_url", &self.base_url)
    }
}

/// Contents of `<config_dir>/qgate.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub dashboard: DashboardConfig,
    pub repos: ReposConfig,
    pub notify: NotifyConfig,
    pub watch: ServerWatchConfig,
    pub llm: ServerLlmConfig,
    /// Default Jira settings for repositories that do not set them.
    pub jira: JiraConfig,
    /// Multi-agent ticket review (ticket analysis, test agents, architecture conformance).
    pub agents: AgentsConfig,
    /// Design checks against Figma frames linked from the ticket's Story.
    pub figma: FigmaConfig,
    /// Review files written after each scan for the developer's coding agent.
    pub output: OutputConfig,
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
        self.output.validate()
    }

    /// Applies the `QGATE_*` / `JIRA_*` overrides from [`ENV_OVERRIDES`]; empty or unset variables are ignored.
    pub fn with_env_overrides(mut self, env: impl Fn(&str) -> Option<String>) -> Self {
        for (entry, value) in active_env_overrides(&env) {
            (entry.apply)(&mut self, &value);
        }
        self
    }
}

/// One environment variable that replaces a server setting. `field` is the dotted path of the setting.
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

/// The single source of truth for env overrides: used both to apply them and to lock the fields they set.
pub const ENV_OVERRIDES: [EnvOverride; 14] = [
    EnvOverride { env_var: ENV_BIND, field: "dashboard.bind", apply: |c, v| c.dashboard.bind = v.into() },
    EnvOverride { env_var: ENV_REPOS_ROOT, field: "repos.roots", apply: |c, v| c.repos.roots = split_list(v) },
    EnvOverride {
        env_var: ENV_WEBHOOK_URL,
        field: "notify.webhook_url",
        apply: |c, v| c.notify.webhook_url = v.into(),
    },
    EnvOverride { env_var: ENV_LLM_BASE_URL, field: "llm.base_url", apply: |c, v| c.llm.base_url = v.into() },
    EnvOverride { env_var: ENV_JIRA_BASE_URL, field: "jira.base_url", apply: |c, v| c.jira.base_url = v.into() },
    EnvOverride { env_var: ENV_JIRA_EMAIL, field: "jira.email", apply: |c, v| c.jira.email = v.into() },
    EnvOverride { env_var: ENV_JIRA_ENABLED, field: "jira.enabled", apply: |c, v| c.jira.enabled = parse_flag(v) },
    EnvOverride { env_var: ENV_OUTPUT_DIR, field: "output.dir", apply: |c, v| c.output.dir = v.into() },
    EnvOverride {
        env_var: ENV_JIRA_MCP_COMMAND,
        field: "jira.mcp.command",
        apply: |c, v| c.jira.mcp.command = v.split_whitespace().map(str::to_string).collect(),
    },
    EnvOverride {
        env_var: ENV_JIRA_MCP_ENV_PASSTHROUGH,
        field: "jira.mcp.env_passthrough",
        apply: |c, v| c.jira.mcp.env_passthrough = split_list(v),
    },
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
];

/// Settings that decide where credentials and code are sent, or which program runs with them. They are
/// never changed over HTTP (settings page, policy assistant): any process that can reach the dashboard
/// could otherwise redirect the API key or run a command. Set them with their env variable (see
/// [`admin_env_var`]) or, before the first start, in the `qgate.toml` seed.
pub const ADMIN_ONLY_FIELDS: [&str; 7] = [
    "llm.base_url",
    "jira.base_url",
    "jira.mcp.command",
    "jira.mcp.env_passthrough",
    "figma.api_base_url",
    "figma.mcp_url",
    "watch.base_branch",
];
/// Write-only and admin-only to set; clearing it over HTTP is allowed (it only stops notifications).
pub const WEBHOOK_URL_FIELD: &str = "notify.webhook_url";
/// Turning repository checks off over HTTP is allowed (tightening); turning them on is admin-only.
pub const RUN_REPO_CHECKS_FIELD: &str = "watch.run_repo_checks";

/// The env variable that sets an (admin-only) field.
pub fn admin_env_var(field: &str) -> Option<&'static str> {
    ENV_OVERRIDES.iter().find(|entry| entry.field == field).map(|entry| entry.env_var)
}

/// Settings read once at startup: changing them needs `docker compose restart qgate`.
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

/// The overrides whose variable is set to a non-empty value, with that (trimmed) value.
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
        assert_eq!(config.llm.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn env_overrides_replace_common_fields() {
        let config = ServerConfig::default().with_env_overrides(env(&[
            (ENV_BIND, "0.0.0.0:9000"),
            (ENV_REPOS_ROOT, "/repos/a, /repos/b,,"),
            (ENV_WEBHOOK_URL, "http://ntfy/qgate"),
            (ENV_LLM_BASE_URL, "http://127.0.0.1:4000"),
            (ENV_JIRA_BASE_URL, "https://acme.atlassian.net"),
            (ENV_JIRA_EMAIL, "bot@acme.com"),
            (ENV_OUTPUT_DIR, "/data/reviews"),
        ]));
        assert_eq!(config.dashboard.bind, "0.0.0.0:9000");
        assert_eq!(config.repos.roots, vec!["/repos/a".to_string(), "/repos/b".to_string()]);
        assert_eq!(config.notify.webhook_url, "http://ntfy/qgate");
        assert_eq!(config.llm.base_url, "http://127.0.0.1:4000");
        assert_eq!(config.jira.base_url, "https://acme.atlassian.net");
        assert_eq!(config.jira.email, "bot@acme.com");
        assert_eq!(config.output.dir, "/data/reviews");
        assert!(config.validate().is_ok());
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
        c.llm.base_url = "http://api.example.com".into();
        cases.push((c, "llm.base_url"));
        let mut c = base.clone();
        c.jira.enabled = true;
        cases.push((c, "jira.email"));
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
        let active = active_env_overrides(&env(&[(ENV_JIRA_EMAIL, " a@b.c "), (ENV_BIND, " ")]));
        assert_eq!(active.len(), 1);
        assert_eq!(
            (active[0].0.field, active[0].0.env_var, active[0].1.as_str()),
            ("jira.email", ENV_JIRA_EMAIL, "a@b.c")
        );
        assert!(format!("{:?}", active[0].0).contains("jira.email"));
        assert_eq!(ENV_OVERRIDES.len(), 14);
    }

    #[test]
    fn admin_only_fields_are_settable_from_env() {
        let config = ServerConfig::default().with_env_overrides(env(&[
            (ENV_JIRA_MCP_COMMAND, " uvx  mcp-atlassian==0.24.0 "),
            (ENV_JIRA_MCP_ENV_PASSTHROUGH, "JIRA_URL, JIRA_API_TOKEN"),
            (ENV_FIGMA_API_BASE_URL, "https://figma.proxy.example"),
            (ENV_FIGMA_MCP_URL, "https://mcp.proxy.example/mcp"),
            (ENV_RUN_REPO_CHECKS, "off"),
            (ENV_WATCH_BASE_BRANCH, "develop"),
        ]));
        assert_eq!(config.jira.mcp.command, vec!["uvx".to_string(), "mcp-atlassian==0.24.0".into()]);
        assert_eq!(config.jira.mcp.env_passthrough, vec!["JIRA_URL".to_string(), "JIRA_API_TOKEN".into()]);
        assert_eq!(config.figma.api_base_url, "https://figma.proxy.example");
        assert_eq!(config.figma.mcp_url, "https://mcp.proxy.example/mcp");
        assert!(!config.watch.run_repo_checks);
        assert_eq!(config.watch.base_branch, "develop");
        assert!(config.validate().is_ok());
        for field in ADMIN_ONLY_FIELDS.iter().chain([&WEBHOOK_URL_FIELD, &RUN_REPO_CHECKS_FIELD]) {
            assert!(admin_env_var(field).is_some(), "{field} needs an env override");
        }
        assert_eq!(admin_env_var("agents.model"), None);
    }
}
