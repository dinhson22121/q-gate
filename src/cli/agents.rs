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
use crate::llm::{CliConfig, MeteredModel, StructuredModel, UsageMeter};
use crate::review::application::{AgentSuite, Category, DesignMismatchKind, GatePolicy};
use crate::sandbox::application::ExecBackend;
use crate::settings::application::{
    AgentEffort, CredentialName, CredentialStore, DesignCheck, FailAction, FigmaProvider, ServerConfig,
    TestLanguage as SettingsLanguage,
};
use crate::settings::infrastructure::resolve_credential;
use crate::testing::application::metered_loop::MeteredToolLoop;
use crate::testing::application::ports::ScenarioSnapshots;
use crate::testing::infrastructure::CliToolLoop;
use crate::testing::{ClaudeTestAgent, TestAgentSettings, TestLanguage};
use crate::tickets::infrastructure::{ClaudeTicketAnalyst, SqliteTicketCache};

const TEST_AGENT_MAX_TURNS: u32 = 30;
const TEST_AGENT_MODEL_TIME: Duration = Duration::from_secs(600);

pub struct Agents {
    pub suite: AgentSuite,
    pub meter: UsageMeter,
    pub notes: Vec<String>,
}

pub fn build_agents(
    server: &ServerConfig,
    model: Arc<dyn StructuredModel>,
    cli: &CliConfig,
    db: &Path,
    store: &dyn CredentialStore,
    exec: Arc<dyn ExecBackend>,
    snapshots: Arc<dyn ScenarioSnapshots>,
) -> Result<Option<Agents>> {
    let config = &server.agents;
    if !config.enabled {
        return Ok(None);
    }
    let meter = UsageMeter::default();
    let model: Arc<dyn StructuredModel> = Arc::new(MeteredModel::new(model, meter.clone()));
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
                    model.clone(),
                    Arc::new(cache),
                    config.tests.max_scenarios as usize,
                )));
            }
            Err(e) => notes.push(format!("ticket analyst disabled: {e}")),
        }
    }
    if config.tests.enabled {
        let settings = test_settings(server);
        let session = test_session_timeout(&settings);
        match CliToolLoop::new(CliConfig { timeout: session, ..cli.clone() }) {
            Ok(driver) => {
                let driver = Arc::new(MeteredToolLoop::new(driver, meter.clone()));
                suite.tests = Some(Arc::new(ClaudeTestAgent::new(driver, settings, exec, snapshots)));
            }
            Err(e) => notes.push(format!("test agent disabled: {e}")),
        }
    }
    if config.architecture.enabled {
        match SqliteRulebookStore::open(db) {
            Ok(store) => {
                let service = Arc::new(RulebookService::new(
                    Arc::new(ClaudeRuleModel::new(model.clone())),
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
        match design_reviewer(server, db, store, model.clone()) {
            Ok(reviewer) => suite.design = Some(reviewer),
            Err(e) => notes.push(format!("design review disabled: {e:#}")),
        }
    }
    Ok(Some(Agents { suite, meter, notes }))
}

pub fn rulebook_service(context: Arc<AppContext>) -> Result<Arc<RulebookService>> {
    let db = context.paths.data_dir.join(HISTORY_DB_FILE);
    let store = SqliteRulebookStore::open(&db).with_context(|| format!("cannot open rulebooks in {}", db.display()))?;
    Ok(Arc::new(RulebookService::new(Arc::new(LiveRuleModel { context }), Arc::new(FsRepoReader), Arc::new(store))))
}

struct LiveRuleModel {
    context: Arc<AppContext>,
}

impl LiveRuleModel {
    fn model(&self) -> Result<ClaudeRuleModel, ConventionsError> {
        let server = self.context.server();
        let model = self
            .context
            .structured_model(&server.agents.model, effort_name(&server))
            .map_err(|e| ConventionsError::Model(e.to_string()))?;
        Ok(ClaudeRuleModel::new(model))
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

pub fn apply_agent_policy(mut policy: GatePolicy, server: &ServerConfig) -> GatePolicy {
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
    model: Arc<dyn StructuredModel>,
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
    let states_llm = server.figma.checks.contains(&DesignCheck::States).then_some(model);
    Ok(Arc::new(FigmaDesignReviewer::new(Arc::new(source), Arc::new(cache), states_llm, checks)))
}

pub(super) fn effort_name(server: &ServerConfig) -> &'static str {
    match server.agents.effort {
        AgentEffort::Low => "low",
        AgentEffort::Medium => "medium",
        AgentEffort::High => "high",
        AgentEffort::Xhigh => "xhigh",
        AgentEffort::Max => "max",
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

fn test_session_timeout(settings: &TestAgentSettings) -> Duration {
    settings.timeout * (settings.max_repair_rounds + 1) + TEST_AGENT_MODEL_TIME
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
    use crate::llm::FakeModel;
    use crate::review::infrastructure::snapshot::GitScenarioSnapshots;
    use crate::sandbox::infrastructure::LocalBackend;
    use crate::settings::infrastructure::FileStore;

    fn build(server: &ServerConfig, dir: &Path, store: &FileStore) -> Option<Agents> {
        let snapshots = Arc::new(GitScenarioSnapshots::new(dir.join("work")));
        let model = Arc::new(FakeModel::new(vec![]));
        let cli = CliConfig::default();
        build_agents(server, model, &cli, &dir.join("q.db"), store, Arc::new(LocalBackend), snapshots).unwrap()
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
        if std::env::var("FIGMA_TOKEN").is_err() && std::env::var("FIGMA_TOKEN_FILE").is_err() {
            assert!(built.notes.iter().any(|n| n.contains("Figma token")));
        }
    }

    #[test]
    fn no_agent_needs_an_api_key_the_test_agent_included() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = ServerConfig::default();
        server.figma.enabled = true;
        server.figma.checks = vec![DesignCheck::States];
        let store = FileStore::new(dir.path().join("c.toml"));

        let built = build(&server, dir.path(), &store).expect("agents on by default");

        assert!(built.suite.analyst.is_some() && built.suite.architecture.is_some());
        assert!(built.suite.tests.is_some(), "the test agent runs on the Claude CLI");
        assert!(!built.notes.iter().any(|n| n.starts_with("test agent disabled")));
        assert!(!built.notes.iter().any(|n| n.starts_with("design state check disabled")));
    }

    #[test]
    fn an_invalid_cli_setting_disables_the_test_agent_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("c.toml"));
        let snapshots = Arc::new(GitScenarioSnapshots::new(dir.path().join("work")));
        let model = Arc::new(FakeModel::new(vec![]));
        let cli = CliConfig { effort: "huge".into(), ..CliConfig::default() };

        let built = build_agents(
            &ServerConfig::default(),
            model,
            &cli,
            &dir.path().join("q.db"),
            &store,
            Arc::new(LocalBackend),
            snapshots,
        )
        .unwrap()
        .unwrap();

        assert!(built.suite.tests.is_none());
        assert!(built.notes.iter().any(|n| n.starts_with("test agent disabled") && n.contains("effort")));
    }

    #[test]
    fn a_test_session_may_run_every_allowed_test_plus_model_time() {
        let settings = TestAgentSettings {
            languages: vec![],
            max_repair_rounds: 2,
            timeout: Duration::from_secs(100),
            max_turns: 30,
        };
        assert_eq!(test_session_timeout(&settings), Duration::from_secs(300) + TEST_AGENT_MODEL_TIME);
    }

    #[test]
    fn effort_names_follow_the_setting() {
        let mut server = ServerConfig::default();
        for (effort, name) in [(AgentEffort::Low, "low"), (AgentEffort::Xhigh, "xhigh"), (AgentEffort::Max, "max")] {
            server.agents.effort = effort;
            assert_eq!(effort_name(&server), name);
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
