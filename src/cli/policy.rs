//! Wires the policy context ("Policy assistant", `/policy`) to the live settings, rulebook and history.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use serde_json::Value;

use super::agents::{apply_agent_policy, claude_config};
use super::pipeline::{AppContext, HISTORY_DB_FILE};
use crate::conventions::application::RulebookService;
use crate::history::application::RunRepository;
use crate::llm::{Budget, ClaudeClient};
use crate::policy::application::{
    GatePolicySource, PolicyDeps, PolicyError, PolicyModel, PolicyPrompt, PolicyService, RepoGate,
};
use crate::policy::domain::ProposalDraft;
use crate::policy::infrastructure::{
    ClaudePolicyModel, HistoryUseCases, RulebookUseCases, SettingsUseCases, SqliteProposalStore,
};
use crate::review::application::GatePolicy;
use crate::settings::application::{CredentialName, ServerConfig, SettingsDeps};
use crate::settings::infrastructure::{load_config, resolve_credential};

/// The policy service on the shared settings dependencies (one write lock with the settings page).
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

/// `PolicyModel` that builds a Claude client from the live settings for every call, so the page works
/// before an API key is configured and picks up a key set later on the Settings page.
struct LivePolicyModel {
    context: Arc<AppContext>,
}

impl LivePolicyModel {
    fn model(&self) -> Result<ClaudePolicyModel, PolicyError> {
        let server = self.context.server();
        let key = resolve_credential(CredentialName::AnthropicApiKey, self.context.store.as_ref())
            .map_err(|e| PolicyError::ModelUnavailable(e.to_string()))?
            .ok_or_else(|| PolicyError::ModelUnavailable("no Anthropic API key configured".into()))?;
        let client = ClaudeClient::new(
            key.expose().to_string(),
            claude_config(&server),
            Budget::new(server.agents.budget_usd_per_review),
        )
        .map_err(|e| PolicyError::ModelUnavailable(e.to_string()))?;
        Ok(ClaudePolicyModel::new(Arc::new(client)))
    }
}

impl PolicyModel for LivePolicyModel {
    fn propose<'a>(&'a self, prompt: &'a PolicyPrompt) -> BoxFuture<'a, Result<ProposalDraft, PolicyError>> {
        Box::pin(async move { self.model()?.propose(prompt).await })
    }
}

/// Reads a repository's working-tree `.qgate.toml` (read-only); defaults when it is missing or invalid.
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::history::infrastructure::SqliteRunRepository;
    use crate::policy::domain::{ApprovalRequest, ProposalStatus};
    use crate::review::application::Severity;
    use crate::settings::application::{ServerLlmConfig, store_secret};
    use crate::settings::infrastructure::{AppPaths, FileStore, SqliteServerConfigStore};

    fn context(dir: &Path, base_url: &str, with_key: bool) -> Arc<AppContext> {
        let store = FileStore::new(dir.join("config/credentials.toml"));
        if with_key {
            store_secret(CredentialName::AnthropicApiKey, "sk-ant-test", &store).unwrap();
        }
        let server = ServerConfig { llm: ServerLlmConfig { base_url: base_url.into() }, ..ServerConfig::default() };
        Arc::new(AppContext {
            paths: AppPaths::with_dirs(dir.join("config"), dir.join("data")),
            server: Arc::new(std::sync::RwLock::new(server)),
            store: Arc::new(store),
            settings_store: Arc::new(SqliteServerConfigStore::open(dir.join("data/qgate.db")).unwrap()),
            env: Arc::new(|_: &str| None),
        })
    }

    fn service(context: Arc<AppContext>) -> Arc<PolicyService> {
        let db = context.paths.data_dir.join(HISTORY_DB_FILE);
        let history = Arc::new(SqliteRunRepository::open(&db).unwrap());
        let rulebooks = super::super::agents::rulebook_service(context.clone()).unwrap();
        policy_service(context.clone(), context.settings_deps(), rulebooks, history).unwrap()
    }

    #[tokio::test]
    async fn live_model_proposes_with_the_configured_key_and_approval_writes_a_settings_version() {
        let mock = MockServer::start().await;
        let answer = json!({
            "summary": "Block on failing generated tests",
            "settings_changes": [{"path": "agents.tests.on_fail", "value_json": "\"block\"", "reason": "asked"}],
            "rule_changes": [], "gate_changes": [], "check_suggestions": [], "unsupported": []
        });
        let reply = ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
            "content": [{"type": "text", "text": answer.to_string()}], "stop_reason": "end_turn", "stop_details": null,
            "usage": {"input_tokens": 10, "output_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
        }));
        Mock::given(method("POST")).and(path("/v1/messages")).respond_with(reply).expect(1).mount(&mock).await;
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path(), &mock.uri(), true);
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
    async fn without_an_api_key_propose_reports_model_unavailable() {
        if std::env::var("ANTHROPIC_API_KEY").is_ok() || std::env::var("ANTHROPIC_API_KEY_FILE").is_ok() {
            return; // a developer key in the environment would be used instead
        }
        let dir = tempfile::tempdir().unwrap();
        let service = service(context(dir.path(), "https://api.anthropic.com", false));
        let result = service.propose("x", None).await;
        assert!(matches!(result, Err(PolicyError::ModelUnavailable(m)) if m.contains("API key")));
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
