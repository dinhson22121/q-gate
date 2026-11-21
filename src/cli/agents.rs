//! Builds the ticket agents for one review from the live server settings.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use futures::future::BoxFuture;

use super::pipeline::{AppContext, HISTORY_DB_FILE};
use crate::conventions::application::{
    CheckRequest, ConventionsError, DraftRequest, RuleDraft, RuleModel, RulebookReviewer, RulebookService,
    ViolationDraft,
};
use crate::conventions::infrastructure::{ClaudeRuleModel, FsRepoReader, SqliteRulebookStore};
use crate::design::application::DesignCheckKind;
use crate::design::infrastructure::{
    FigmaDesignReviewer, FigmaRestConfig, FigmaRestSource, FigmaToken, SqliteDesignCache,
};
use crate::llm::{Budget, ClaudeClient, ClaudeConfig};
use crate::review::application::{AgentSuite, Category, DesignMismatchKind, GatePolicy};
use crate::sandbox::application::ExecBackend;
use crate::settings::application::{
    AgentEffort, CredentialName, CredentialStore, DesignCheck, FailAction, FigmaProvider, Secret, ServerConfig,
    TestLanguage as SettingsLanguage,
};
use crate::settings::infrastructure::resolve_credential;
use crate::testing::application::ports::ScenarioSnapshots;
use crate::testing::{ClaudeTestAgent, TestAgentSettings, TestLanguage};
use crate::tickets::infrastructure::{ClaudeTicketAnalyst, SqliteTicketCache};

/// Requests one test agent may send to Claude for its scenario.
const TEST_AGENT_MAX_TURNS: u32 = 30;

/// The agents, their shared spend tracker, and notes about agents that could not be enabled.
pub struct Agents {
    pub suite: AgentSuite,
    pub budget: Budget,
    pub notes: Vec<String>,
}

/// `None` when agents are disabled. Unavailable agents (missing token, cache error) become notes.
/// Test agents run their tests through `exec`, each in its own snapshot from `snapshots`.
pub fn build_agents(
    server: &ServerConfig,
    api_key: &Secret,
    db: &Path,
    store: &dyn CredentialStore,
    exec: Arc<dyn ExecBackend>,
    snapshots: Arc<dyn ScenarioSnapshots>,
) -> Result<Option<Agents>> {
    let config = &server.agents;
    if !config.enabled {
        return Ok(None);
    }
    let budget = Budget::new(config.budget_usd_per_review);
    let client = Arc::new(
        ClaudeClient::new(api_key.expose().to_string(), claude_config(server), budget.clone())
            .context("cannot create the Claude client for agents")?,
    );
    let mut notes = Vec::new();
    let mut suite = AgentSuite {
        max_parallel: config.max_parallel,
        max_scenarios: config.tests.max_scenarios as usize,
        ..AgentSuite::default()
    };
    if config.ticket_analyst.enabled {
        match SqliteTicketCache::open(db) {
            Ok(cache) => {
                suite.analyst = Some(Arc::new(ClaudeTicketAnalyst::new(
                    client.clone(),
                    Arc::new(cache),
                    config.tests.max_scenarios as usize,
                )));
            }
            Err(e) => notes.push(format!("ticket analyst disabled: {e}")),
        }
    }
    if config.tests.enabled {
        suite.tests = Some(Arc::new(ClaudeTestAgent::new(client.clone(), test_settings(server), exec, snapshots)));
    }
    if config.architecture.enabled {
        match SqliteRulebookStore::open(db) {
            Ok(store) => {
                let service = Arc::new(RulebookService::new(
                    Arc::new(ClaudeRuleModel::new(client.clone())),
                    Arc::new(FsRepoReader),
                    Arc::new(store),
                ));
                suite.architecture = Some(Arc::new(RulebookReviewer::with_service(
                    service,
                    config.architecture.use_inferred_rules,
                    config.architecture.rulebook_refresh_days,
                )));
            }
            Err(e) => notes.push(format!("architecture review disabled: {e}")),
        }
    }
    if server.figma.enabled {
        match design_reviewer(server, db, store, client.clone()) {
            Ok(reviewer) => suite.design = Some(reviewer),
            Err(e) => notes.push(format!("design review disabled: {e:#}")),
        }
    }
    Ok(Some(Agents { suite, budget, notes }))
}

/// Rulebook service for the `/rulebook` page. The API key and model are resolved at each call, so the
/// page works before a key is configured and picks up a key set later on the Settings page.
pub fn rulebook_service(context: Arc<AppContext>) -> Result<Arc<RulebookService>> {
    let db = context.paths.data_dir.join(HISTORY_DB_FILE);
    let store = SqliteRulebookStore::open(&db).with_context(|| format!("cannot open rulebooks in {}", db.display()))?;
    Ok(Arc::new(RulebookService::new(Arc::new(LiveRuleModel { context }), Arc::new(FsRepoReader), Arc::new(store))))
}

/// `RuleModel` that builds a Claude client from the live settings for every call.
struct LiveRuleModel {
    context: Arc<AppContext>,
}

impl LiveRuleModel {
    fn model(&self) -> Result<ClaudeRuleModel, ConventionsError> {
        let server = self.context.server();
        let key = resolve_credential(CredentialName::AnthropicApiKey, self.context.store.as_ref())
            .map_err(|e| ConventionsError::Model(e.to_string()))?
            .ok_or_else(|| ConventionsError::Model("no Anthropic API key configured".into()))?;
        let budget = Budget::new(server.agents.budget_usd_per_review);
        let client = ClaudeClient::new(key.expose().to_string(), claude_config(&server), budget)
            .map_err(|e| ConventionsError::Model(e.to_string()))?;
        Ok(ClaudeRuleModel::new(Arc::new(client)))
    }
}

impl RuleModel for LiveRuleModel {
    fn draft_rules<'a>(&'a self, request: &'a DraftRequest) -> BoxFuture<'a, Result<Vec<RuleDraft>, ConventionsError>> {
        Box::pin(async move { self.model()?.draft_rules(request).await })
    }

    fn find_violations<'a>(
        &'a self,
        request: CheckRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<ViolationDraft>, ConventionsError>> {
        Box::pin(async move { self.model()?.find_violations(request).await })
    }
}

/// The server's agent policies on top of the repository's gate policy.
pub fn apply_agent_policy(mut policy: GatePolicy, server: &ServerConfig) -> GatePolicy {
    // Tool findings (import boundaries) are graded by the tools; an older repo config must not mute them.
    if !policy.categories.contains(&Category::Architecture) {
        policy.categories.push(Category::Architecture);
    }
    policy.fail_on_test_failure = server.agents.enabled && server.agents.tests.on_fail == FailAction::Block;
    policy.fail_on_rule_violation =
        server.agents.enabled && server.agents.architecture.on_violation == FailAction::Block;
    policy.fail_on_design = if server.figma.enabled {
        server.figma.block_on.iter().filter_map(|c| mismatch_kind(*c)).collect()
    } else {
        Vec::new()
    };
    policy
}

fn design_reviewer(
    server: &ServerConfig,
    db: &Path,
    store: &dyn CredentialStore,
    client: Arc<ClaudeClient>,
) -> Result<Arc<FigmaDesignReviewer>> {
    anyhow::ensure!(
        server.figma.provider == FigmaProvider::Rest,
        "Figma's cloud MCP needs OAuth from a catalog-listed client; use provider = \"rest\""
    );
    let token = resolve_credential(CredentialName::FigmaToken, store)?
        .context("no Figma token: set it on the Settings page or FIGMA_TOKEN")?;
    let source = FigmaRestSource::new(FigmaRestConfig {
        api_base_url: server.figma.api_base_url.clone(),
        token: FigmaToken::new(token.expose()),
    })?;
    let cache = SqliteDesignCache::open(db)?;
    let checks: Vec<DesignCheckKind> = server.figma.checks.iter().map(|c| check_kind(*c)).collect();
    let states_llm = server.figma.checks.contains(&DesignCheck::States).then_some(client);
    Ok(Arc::new(FigmaDesignReviewer::new(Arc::new(source), Arc::new(cache), states_llm, checks)))
}

pub(super) fn claude_config(server: &ServerConfig) -> ClaudeConfig {
    let effort = match server.agents.effort {
        AgentEffort::Low => "low",
        AgentEffort::Medium => "medium",
        AgentEffort::High => "high",
        AgentEffort::Xhigh => "xhigh",
        AgentEffort::Max => "max",
    };
    ClaudeConfig {
        base_url: server.llm.base_url.clone(),
        model: server.agents.model.clone(),
        effort: effort.into(),
        ..ClaudeConfig::default()
    }
}

fn test_settings(server: &ServerConfig) -> TestAgentSettings {
    let tests = &server.agents.tests;
    TestAgentSettings {
        languages: tests.languages.iter().map(|l| language(*l)).collect(),
        max_repair_rounds: tests.max_repair_rounds,
        timeout: Duration::from_secs(tests.timeout_secs),
        max_turns: TEST_AGENT_MAX_TURNS,
    }
}

fn language(language: SettingsLanguage) -> TestLanguage {
    match language {
        SettingsLanguage::Go => TestLanguage::Go,
        SettingsLanguage::Java => TestLanguage::Java,
        SettingsLanguage::Python => TestLanguage::Python,
        SettingsLanguage::Rust => TestLanguage::Rust,
    }
}

fn check_kind(check: DesignCheck) -> DesignCheckKind {
    match check {
        DesignCheck::Copy => DesignCheckKind::Copy,
        DesignCheck::Component => DesignCheckKind::Component,
        DesignCheck::Token => DesignCheckKind::Token,
        DesignCheck::States => DesignCheckKind::States,
    }
}

/// `States` is judged by the model alone and never blocks.
fn mismatch_kind(check: DesignCheck) -> Option<DesignMismatchKind> {
    match check {
        DesignCheck::Copy => Some(DesignMismatchKind::Copy),
        DesignCheck::Component => Some(DesignMismatchKind::Component),
        DesignCheck::Token => Some(DesignMismatchKind::Token),
        DesignCheck::States => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::snapshot::GitScenarioSnapshots;
    use crate::sandbox::infrastructure::LocalBackend;
    use crate::settings::infrastructure::FileStore;

    fn build(server: &ServerConfig, dir: &Path, store: &FileStore) -> Option<Agents> {
        let snapshots = Arc::new(GitScenarioSnapshots::new(dir.join("work")));
        build_agents(server, &Secret::new("sk-ant-test"), &dir.join("q.db"), store, Arc::new(LocalBackend), snapshots)
            .unwrap()
    }

    #[test]
    fn architecture_findings_can_always_block_even_with_an_older_category_list() {
        let narrow = GatePolicy { categories: vec![Category::Security], ..GatePolicy::default() };
        let policy = apply_agent_policy(narrow, &ServerConfig::default());
        assert!(policy.categories.contains(&Category::Architecture));
        assert_eq!(policy.categories.iter().filter(|c| **c == Category::Architecture).count(), 1);
        let again = apply_agent_policy(policy.clone(), &ServerConfig::default());
        assert_eq!(again.categories, policy.categories, "applying twice does not duplicate");
    }

    #[test]
    fn disabled_agents_build_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = ServerConfig::default();
        server.agents.enabled = false;
        let store = FileStore::new(dir.path().join("c.toml"));
        assert!(build(&server, dir.path(), &store).is_none());
    }

    #[test]
    fn default_agents_are_built_and_figma_without_token_is_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = ServerConfig::default();
        server.figma.enabled = true;
        let store = FileStore::new(dir.path().join("c.toml"));
        let built = build(&server, dir.path(), &store).expect("agents on by default");
        assert!(built.suite.analyst.is_some() && built.suite.tests.is_some() && built.suite.architecture.is_some());
        assert!(built.suite.design.is_none());
        // FIGMA_TOKEN may exist in a developer's env; only assert the note when it does not.
        if std::env::var("FIGMA_TOKEN").is_err() && std::env::var("FIGMA_TOKEN_FILE").is_err() {
            assert!(built.notes.iter().any(|n| n.contains("Figma token")));
        }
    }

    #[test]
    fn agent_policy_follows_server_settings() {
        let mut server = ServerConfig::default();
        let lenient = apply_agent_policy(GatePolicy::default(), &server);
        assert!(!lenient.fail_on_test_failure && !lenient.fail_on_rule_violation && lenient.fail_on_design.is_empty());
        server.agents.tests.on_fail = FailAction::Block;
        server.agents.architecture.on_violation = FailAction::Block;
        server.figma.enabled = true;
        server.figma.block_on = vec![DesignCheck::Copy, DesignCheck::States];
        let strict = apply_agent_policy(GatePolicy::default(), &server);
        assert!(strict.fail_on_test_failure && strict.fail_on_rule_violation);
        assert_eq!(strict.fail_on_design, vec![DesignMismatchKind::Copy]);
    }
}
