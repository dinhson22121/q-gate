use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use serde_json::Value;

use super::agents::{apply_agent_policy, effort_name};
use super::pipeline::{AppContext, HISTORY_DB_FILE};
use crate::conventions::application::RulebookService;
use crate::history::application::RunRepository;
use crate::policy::application::{
    GatePolicySource, PolicyDeps, PolicyError, PolicyModel, PolicyPrompt, PolicyService, RepoGate,
};
use crate::policy::domain::ProposalDraft;
use crate::policy::infrastructure::{
    ClaudePolicyModel, HistoryUseCases, RulebookUseCases, SettingsUseCases, SqliteProposalStore,
};
use crate::review::application::GatePolicy;
use crate::settings::application::{ServerConfig, SettingsDeps};
use crate::settings::infrastructure::load_config;

pub fn policy_service(
    context: Arc<AppContext>,
    settings: Arc<SettingsDeps>,
    rulebooks: Arc<RulebookService>,
    history: Arc<dyn RunRepository>,
) -> Result<Arc<PolicyService>> {
    let db = context.paths.data_dir.join(HISTORY_DB_FILE);
    let store = SqliteProposalStore::open(&db).with_context(|| format!("cannot open proposals in {}", db.display()))?;
    Ok(Arc::new(PolicyService::new(PolicyDeps {
        model: Arc::new(LivePolicyModel { context }),
        settings: Arc::new(SettingsUseCases::new(settings)),
        rulebooks: Arc::new(RulebookUseCases::new(rulebooks)),
        history: Arc::new(HistoryUseCases::new(history)),
        gates: Arc::new(RepoGates),
        store: Arc::new(store),
        now: chrono::Utc::now,
    })))
}

struct LivePolicyModel {
    context: Arc<AppContext>,
}

impl LivePolicyModel {
    fn model(&self) -> Result<ClaudePolicyModel, PolicyError> {
        let server = self.context.server();
        let model = self
            .context
            .structured_model(&server.agents.model, effort_name(&server))
            .map_err(|e| PolicyError::ModelUnavailable(e.to_string()))?;
        Ok(ClaudePolicyModel::new(model))
    }
}

impl PolicyModel for LivePolicyModel {
    fn propose<'a>(&'a self, prompt: &'a PolicyPrompt) -> BoxFuture<'a, Result<ProposalDraft, PolicyError>> {
        Box::pin(async move { self.model()?.propose(prompt).await })
    }
}

struct RepoGates;

impl GatePolicySource for RepoGates {
    fn repo_gate(&self, repo_path: &str) -> RepoGate {
        match load_config(Path::new(repo_path)) {
            Ok(config) => RepoGate {
                policy: config.gate_policy(),
                check_names: config.checks.into_iter().map(|c| c.name).collect(),
            },
            Err(_) => RepoGate::default(),
        }
    }

    fn with_server(&self, policy: GatePolicy, settings: &Value) -> GatePolicy {
        match serde_json::from_value::<ServerConfig>(settings.clone()) {
            Ok(server) => apply_agent_policy(policy, &server),
            Err(_) => policy,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::history::infrastructure::SqliteRunRepository;
    use crate::llm::{FakeModel, StructuredModel};
    use crate::policy::domain::{ApprovalRequest, ProposalStatus};
    use crate::review::application::Severity;
    use crate::settings::application::EnvLookup;
    use crate::settings::infrastructure::{AppPaths, FileStore, SqliteServerConfigStore};

    fn context(dir: &Path, model: Option<Arc<dyn StructuredModel>>, env: EnvLookup) -> Arc<AppContext> {
        let store = FileStore::new(dir.join("config/credentials.toml"));
        let server = ServerConfig::default();
        Arc::new(AppContext {
            paths: AppPaths::with_dirs(dir.join("config"), dir.join("data")),
            server: Arc::new(std::sync::RwLock::new(server)),
            store: Arc::new(store),
            settings_store: Arc::new(SqliteServerConfigStore::open(dir.join("data/qgate.db")).unwrap()),
            env,
            model_override: model,
        })
    }

    fn service(context: Arc<AppContext>) -> Arc<PolicyService> {
        let db = context.paths.data_dir.join(HISTORY_DB_FILE);
        let history = Arc::new(SqliteRunRepository::open(&db).unwrap());
        let rulebooks = super::super::agents::rulebook_service(context.clone()).unwrap();
        policy_service(context.clone(), context.settings_deps(), rulebooks, history).unwrap()
    }

    #[tokio::test]
    async fn live_model_proposes_and_approval_writes_a_settings_version() {
        let answer = json!({
            "summary": "Block on failing generated tests",
            "settings_changes": [{"path": "agents.tests.on_fail", "value_json": "\"block\"", "reason": "asked"}],
            "rule_changes": [], "gate_changes": [], "check_suggestions": [], "unsupported": []
        });
        let dir = tempfile::tempdir().unwrap();
        let fake: Arc<dyn StructuredModel> = Arc::new(FakeModel::answering(answer));
        let context = context(dir.path(), Some(fake), Arc::new(|_: &str| None));
        let service = service(context.clone());
        let view = service.propose("Failing generated tests must block.", None).await.unwrap();
        assert_eq!(view.proposal.content.settings[0].path, "agents.tests.on_fail");
        let request = ApprovalRequest { settings: vec![0], ..ApprovalRequest::default() };
        let applied = service.approve(view.proposal.id, request).await.unwrap();
        assert_eq!(applied.proposal.status, ProposalStatus::Applied);
        assert_eq!(context.server().agents.tests.on_fail, crate::settings::application::FailAction::Block);
        let rows = context.settings_store.recent(1).unwrap();
        assert_eq!(rows[0].changed_by, format!("policy-assistant:p{}", view.proposal.id));
    }

    #[tokio::test]
    async fn a_missing_claude_binary_is_reported_as_a_model_error() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path(), None, Arc::new(|_: &str| None));
        context.server.write().unwrap().llm.claude_bin = "/definitely/missing/claude".into();
        let service = service(context);
        let result = service.propose("x", None).await;
        assert!(matches!(result, Err(PolicyError::Model(ref m)) if m.contains("cannot start")), "{result:?}");
    }

    #[test]
    fn repo_gates_read_qgate_toml_and_apply_server_agent_policies() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".qgate.toml"),
            "[gate]\nfail_on = \"major\"\n\n[[checks]]\nname = \"unit\"\ncommand = [\"go\", \"test\"]\n",
        )
        .unwrap();
        let gate = RepoGates.repo_gate(dir.path().to_str().unwrap());
        assert_eq!(gate.policy.fail_on, Severity::Major);
        assert_eq!(gate.check_names, vec!["unit"]);
        assert_eq!(RepoGates.repo_gate("/definitely/missing"), RepoGate::default());

        let mut settings = serde_json::to_value(ServerConfig::default()).unwrap();
        settings["agents"]["tests"]["on_fail"] = json!("block");
        assert!(RepoGates.with_server(GatePolicy::default(), &settings).fail_on_test_failure);
        assert_eq!(RepoGates.with_server(GatePolicy::default(), &json!(1)), GatePolicy::default());
    }
}
