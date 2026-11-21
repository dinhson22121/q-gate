//! Server-level settings for the multi-agent ticket review: ticket analysis, test agents,
//! architecture conformance and Figma design checks.

use serde::{Deserialize, Serialize};

use super::validation::{ValidationError, ValidationResult, require_range, require_secure_url};

pub const DEFAULT_AGENT_MODEL: &str = "claude-opus-5-5";
const MAX_PARALLEL_AGENTS: usize = 8;
const MAX_SCENARIOS: u32 = 30;
const MAX_REPAIR_ROUNDS: u32 = 6;
const MAX_TEST_TIMEOUT_SECS: u64 = 3600;
const MAX_BUDGET_USD: f64 = 50.0;
const MAX_REFRESH_DAYS: u32 = 90;
const MAX_TTL_MINUTES: u32 = 24 * 60;

/// Languages the test agent can write and run tests for. Frontend (JS/TS) comes later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestLanguage {
    Go,
    Java,
    Python,
    Rust,
}

impl TestLanguage {
    pub const ALL: [Self; 4] = [Self::Go, Self::Java, Self::Python, Self::Rust];
}

/// What a failing generated test does to the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailAction {
    /// Shown on the dashboard with full evidence; the gate is not affected.
    Warn,
    /// Fails the gate. Only failures that carry complete evidence count; others are inconclusive.
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentsConfig {
    pub enabled: bool,
    pub model: String,
    pub effort: AgentEffort,
    /// Sub-agents running at the same time for one review.
    pub max_parallel: usize,
    /// Hard stop for one review's agent spend; remaining agents are skipped and reported.
    pub budget_usd_per_review: f64,
    pub ticket_analyst: TicketAnalystConfig,
    pub tests: TestAgentConfig,
    pub architecture: ArchitectureConfig,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: DEFAULT_AGENT_MODEL.into(),
            effort: AgentEffort::High,
            max_parallel: 3,
            budget_usd_per_review: 2.0,
            ticket_analyst: TicketAnalystConfig::default(),
            tests: TestAgentConfig::default(),
            architecture: ArchitectureConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TicketAnalystConfig {
    pub enabled: bool,
    /// Walk Sub-task → Story → Epic and merge their acceptance criteria and design links.
    pub include_parent_story: bool,
    /// Pass the ticket description to reviewers as context (never gating).
    pub use_description: bool,
    /// Re-fetch a ticket after this many minutes even if Jira's `updated` is unchanged.
    pub cache_ttl_minutes: u32,
}

impl Default for TicketAnalystConfig {
    fn default() -> Self {
        Self { enabled: true, include_parent_story: true, use_description: true, cache_ttl_minutes: 10 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestAgentConfig {
    pub enabled: bool,
    pub languages: Vec<TestLanguage>,
    /// Test scenarios generated per ticket (one agent each).
    pub max_scenarios: u32,
    /// Times an agent may fix its own test after a compile/setup error.
    pub max_repair_rounds: u32,
    pub timeout_secs: u64,
    pub on_fail: FailAction,
}

impl Default for TestAgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            languages: TestLanguage::ALL.to_vec(),
            max_scenarios: 8,
            max_repair_rounds: 3,
            timeout_secs: 300,
            on_fail: FailAction::Warn,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureConfig {
    pub enabled: bool,
    /// Rebuild a repository's cached rulebook after this many days even if no rule file changed.
    pub rulebook_refresh_days: u32,
    /// Inferred (not yet approved) rules are reported but never block.
    pub use_inferred_rules: bool,
    pub on_violation: FailAction,
}

impl Default for ArchitectureConfig {
    fn default() -> Self {
        Self { enabled: true, rulebook_refresh_days: 7, use_inferred_rules: true, on_violation: FailAction::Warn }
    }
}

/// How design data is read. Figma's cloud MCP server only accepts OAuth from catalog-listed clients,
/// so a background service uses the REST API with a personal access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FigmaProvider {
    Rest,
    Mcp,
}

/// Kinds of design mismatch. See `DesignCheckConfig` for which ones run and which block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignCheck {
    /// Text in the frame missing from the changed code and its i18n files.
    Copy,
    /// The design uses design-system component X but the code renders something else.
    Component,
    /// Hard-coded colour/spacing/font where the design binds a variable.
    Token,
    /// Design variants (loading, empty, error, disabled) the code does not handle.
    States,
}

impl DesignCheck {
    pub const ALL: [Self; 4] = [Self::Copy, Self::Component, Self::Token, Self::States];
    /// `States` is judged by the model from code alone, so it can never block.
    pub const BLOCKABLE: [Self; 3] = [Self::Copy, Self::Component, Self::Token];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FigmaConfig {
    pub enabled: bool,
    pub provider: FigmaProvider,
    pub api_base_url: String,
    /// Only used with `provider = "mcp"`.
    pub mcp_url: String,
    pub checks: Vec<DesignCheck>,
    pub block_on: Vec<DesignCheck>,
}

impl Default for FigmaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: FigmaProvider::Rest,
            api_base_url: "https://api.figma.com".into(),
            mcp_url: "https://mcp.figma.com/mcp".into(),
            checks: DesignCheck::ALL.to_vec(),
            block_on: Vec::new(),
        }
    }
}

impl AgentsConfig {
    pub fn validate(&self) -> ValidationResult {
        if self.model.trim().is_empty() {
            return Err(ValidationError::new("agents.model", "must not be empty"));
        }
        require_range("agents.max_parallel", self.max_parallel, 1, MAX_PARALLEL_AGENTS)?;
        if !(self.budget_usd_per_review > 0.0 && self.budget_usd_per_review <= MAX_BUDGET_USD) {
            return Err(ValidationError::new(
                "agents.budget_usd_per_review",
                format!("must be greater than 0 and at most {MAX_BUDGET_USD}"),
            ));
        }
        require_range(
            "agents.ticket_analyst.cache_ttl_minutes",
            self.ticket_analyst.cache_ttl_minutes,
            1,
            MAX_TTL_MINUTES,
        )?;
        require_range("agents.tests.max_scenarios", self.tests.max_scenarios, 1, MAX_SCENARIOS)?;
        require_range("agents.tests.max_repair_rounds", self.tests.max_repair_rounds, 0, MAX_REPAIR_ROUNDS)?;
        require_range("agents.tests.timeout_secs", self.tests.timeout_secs, 10, MAX_TEST_TIMEOUT_SECS)?;
        if self.tests.enabled && self.tests.languages.is_empty() {
            return Err(ValidationError::new(
                "agents.tests.languages",
                "pick at least one language or disable test agents",
            ));
        }
        require_range(
            "agents.architecture.rulebook_refresh_days",
            self.architecture.rulebook_refresh_days,
            1,
            MAX_REFRESH_DAYS,
        )
    }
}

impl FigmaConfig {
    pub fn validate(&self) -> ValidationResult {
        require_secure_url("figma.api_base_url", &self.api_base_url)?;
        require_secure_url("figma.mcp_url", &self.mcp_url)?;
        if let Some(check) = self.block_on.iter().find(|c| !DesignCheck::BLOCKABLE.contains(c)) {
            return Err(ValidationError::new(
                "figma.block_on",
                format!("{check:?} is judged by the model and cannot block"),
            ));
        }
        if let Some(check) = self.block_on.iter().find(|c| !self.checks.contains(c)) {
            return Err(ValidationError::new(
                "figma.block_on",
                format!("{check:?} must also be listed in figma.checks"),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_conservative() {
        let agents = AgentsConfig::default();
        assert!(agents.validate().is_ok());
        assert_eq!(agents.tests.on_fail, FailAction::Warn);
        assert_eq!(agents.architecture.on_violation, FailAction::Warn);
        let figma = FigmaConfig::default();
        assert!(figma.validate().is_ok());
        assert!(!figma.enabled);
        assert!(figma.block_on.is_empty());
    }

    #[test]
    fn rejects_out_of_range_values() {
        let bad = |f: fn(&mut AgentsConfig)| {
            let mut c = AgentsConfig::default();
            f(&mut c);
            c.validate().unwrap_err().field
        };
        assert_eq!(bad(|c| c.max_parallel = 0), "agents.max_parallel");
        assert_eq!(bad(|c| c.budget_usd_per_review = 0.0), "agents.budget_usd_per_review");
        assert_eq!(bad(|c| c.tests.max_scenarios = 99), "agents.tests.max_scenarios");
        assert_eq!(bad(|c| c.tests.languages.clear()), "agents.tests.languages");
        assert_eq!(bad(|c| c.model = " ".into()), "agents.model");
    }

    #[test]
    fn states_check_cannot_block_and_blocking_checks_must_run() {
        let states = FigmaConfig { block_on: vec![DesignCheck::States], ..FigmaConfig::default() };
        assert_eq!(states.validate().unwrap_err().field, "figma.block_on");
        let not_run = FigmaConfig {
            checks: vec![DesignCheck::Copy],
            block_on: vec![DesignCheck::Token],
            ..FigmaConfig::default()
        };
        assert_eq!(not_run.validate().unwrap_err().field, "figma.block_on");
        let ok = FigmaConfig { block_on: vec![DesignCheck::Copy, DesignCheck::Component], ..FigmaConfig::default() };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn figma_urls_must_be_secure() {
        let http = FigmaConfig { api_base_url: "http://api.figma.com".into(), ..FigmaConfig::default() };
        assert_eq!(http.validate().unwrap_err().field, "figma.api_base_url");
    }
}
