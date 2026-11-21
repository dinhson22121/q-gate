//! `SettingsGateway` on the settings context's use cases: every change is validated, env-locked fields are
//! refused and an approval becomes a new, versioned settings entry (rollback works from Settings → History).

use std::sync::Arc;

use serde_json::Value;

use crate::policy::application::{PolicyError, SettingsApplied, SettingsGateway, SettingsSnapshot};
use crate::policy::domain::WEBHOOK_FIELD;
use crate::policy::domain::json::set;
use crate::settings::application::server_settings::{GetSettings, UpdateSettings, WebhookChange, latest_version};
use crate::settings::application::{ServerConfig, SettingsDeps, SettingsError};

pub struct SettingsUseCases {
    deps: Arc<SettingsDeps>,
}

impl SettingsUseCases {
    /// Share the settings page's `deps` so both serialize their writes on the same lock.
    pub fn new(deps: Arc<SettingsDeps>) -> Self {
        Self { deps }
    }
}

fn policy_error(err: SettingsError) -> PolicyError {
    match err {
        SettingsError::Invalid(e) => PolicyError::Invalid(e.to_string()),
        SettingsError::AdminOnly { .. } => PolicyError::Invalid(err.to_string()),
        SettingsError::Locked { .. } | SettingsError::Conflict(_) => PolicyError::Conflict(err.to_string()),
        SettingsError::VersionNotFound(_) => PolicyError::NotFound(err.to_string()),
        SettingsError::Storage(detail) => PolicyError::Storage(detail),
        other @ (SettingsError::SecretFromEnv { .. } | SettingsError::InvalidSecret(_)) => {
            PolicyError::Invalid(other.to_string())
        }
    }
}

fn to_json(config: &ServerConfig) -> Result<Value, PolicyError> {
    serde_json::to_value(config).map_err(|e| PolicyError::Storage(format!("cannot serialize settings: {e}")))
}

fn from_json(config: Value) -> Result<ServerConfig, String> {
    serde_json::from_value(config).map_err(|e| format!("invalid value: {e}"))
}

impl SettingsGateway for SettingsUseCases {
    fn snapshot(&self) -> Result<SettingsSnapshot, PolicyError> {
        let view = GetSettings::new(&self.deps).execute().map_err(policy_error)?;
        Ok(SettingsSnapshot {
            version: latest_version(&self.deps).map_err(policy_error)?,
            config: to_json(&view.config)?,
            locked: view.locked.iter().map(|l| l.field.to_string()).collect(),
            restart_required: view.restart_required_fields.iter().map(|f| f.to_string()).collect(),
            webhook_configured: view.webhook_configured,
        })
    }

    fn validate(&self, config: &Value) -> Result<(), String> {
        from_json(config.clone())?.validate().map_err(|e| e.to_string())
    }

    fn apply(
        &self,
        changes: &[(String, Value)],
        changed_by: &str,
        expected_version: i64,
    ) -> Result<SettingsApplied, PolicyError> {
        let view = GetSettings::new(&self.deps).execute().map_err(policy_error)?;
        let mut config = to_json(&view.config)?;
        let mut webhook = WebhookChange::Keep;
        for (path, value) in changes {
            if path == WEBHOOK_FIELD {
                let url = value.as_str().unwrap_or_default().trim().to_string();
                webhook = if url.is_empty() { WebhookChange::Clear } else { WebhookChange::Set(url) };
            } else if !set(&mut config, path, value.clone()) {
                return Err(PolicyError::Invalid(format!("unknown setting {path}")));
            }
        }
        let patch = from_json(config).map_err(PolicyError::Invalid)?;
        let applied = UpdateSettings::new(&self.deps)
            .execute_as(patch, webhook, changed_by, expected_version)
            .map_err(policy_error)?;
        Ok(SettingsApplied {
            version: applied.version,
            changed_fields: applied.changed_fields,
            restart_required: applied.restart_required,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;

    use serde_json::json;

    use super::*;
    use crate::settings::application::credentials::fakes::MemoryStore;
    use crate::settings::application::server_settings::fakes::{FixedSeed, MemoryConfigStore, env};
    use crate::settings::application::{LoadEffectiveSettings, ServerConfigStore};

    fn gateway(pairs: &[(&str, &str)]) -> (SettingsUseCases, Arc<MemoryConfigStore>) {
        let store = Arc::new(MemoryConfigStore::default());
        let environment = env(pairs);
        let effective = LoadEffectiveSettings::new(store.clone(), Arc::new(FixedSeed(None)), environment.clone())
            .execute()
            .unwrap();
        let deps = SettingsDeps::new(
            store.clone(),
            Arc::new(MemoryStore::default()),
            Arc::new(RwLock::new(effective)),
            environment,
        );
        (SettingsUseCases::new(Arc::new(deps)), store)
    }

    #[test]
    fn snapshot_validate_and_apply_go_through_the_settings_use_cases() {
        let (gateway, store) = gateway(&[("QGATE_LLM_BASE_URL", "http://127.0.0.1:4000")]);
        let snapshot = gateway.snapshot().unwrap();
        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.locked, vec!["llm.base_url".to_string()]);
        assert!(!snapshot.webhook_configured);
        assert!(snapshot.restart_required.contains(&"output.dir".to_string()));

        let mut over_budget = snapshot.config.clone();
        set(&mut over_budget, "agents.budget_usd_per_review", json!(99.0));
        assert!(gateway.validate(&over_budget).unwrap_err().contains("agents.budget_usd_per_review"));
        set(&mut over_budget, "agents.budget_usd_per_review", json!("lots"));
        assert!(gateway.validate(&over_budget).unwrap_err().contains("invalid value"));
        assert!(gateway.validate(&snapshot.config).is_ok());

        let set_webhook = [(WEBHOOK_FIELD.to_string(), json!("https://hooks.example/abc"))];
        let refused = gateway.apply(&set_webhook, "policy-assistant:p1", 1);
        assert!(matches!(refused, Err(PolicyError::Invalid(m)) if m.contains("admin-only")));
        let changes = vec![("agents.tests.on_fail".to_string(), json!("block"))];
        let applied = gateway.apply(&changes, "policy-assistant:p1", 1).unwrap();
        assert_eq!(applied.version, Some(2));
        assert_eq!(applied.changed_fields, vec!["agents.tests.on_fail".to_string()]);
        assert_eq!(store.rows.lock().unwrap()[1].changed_by, "policy-assistant:p1");
        assert!(!gateway.snapshot().unwrap().webhook_configured);

        let stale = gateway.apply(&changes, "policy-assistant:p2", 1);
        assert!(matches!(stale, Err(PolicyError::Conflict(_))));
        let mut with_webhook = store.load_raw().unwrap().unwrap();
        with_webhook.notify.webhook_url = "https://hooks.example/abc".into();
        store.save_raw(&with_webhook, "admin").unwrap();
        assert!(gateway.snapshot().unwrap().webhook_configured);
        let cleared = gateway.apply(&[(WEBHOOK_FIELD.to_string(), json!(""))], "p", 3).unwrap();
        assert_eq!(cleared.version, Some(4));
        assert!(!gateway.snapshot().unwrap().webhook_configured);
        assert!(matches!(gateway.apply(&[("nope.x".into(), json!(1))], "p", 4), Err(PolicyError::Invalid(_))));
        let admin_only = gateway.apply(&[("llm.base_url".into(), json!("https://other.example"))], "p", 4);
        assert!(matches!(admin_only, Err(PolicyError::Invalid(m)) if m.contains("QGATE_LLM_BASE_URL")));
        let invalid = gateway.apply(&[("agents.max_parallel".into(), json!(0))], "p", 4);
        assert!(matches!(invalid, Err(PolicyError::Invalid(_))));
        assert!(matches!(
            gateway.apply(&[("agents.max_parallel".into(), json!("x"))], "p", 4),
            Err(PolicyError::Invalid(_))
        ));
        let off = gateway.apply(&[("watch.run_repo_checks".into(), json!(false))], "p", 4);
        assert!(off.is_ok(), "turning repository checks off is allowed");
    }

    #[test]
    fn maps_every_settings_error() {
        let cases = [
            (SettingsError::VersionNotFound(3), "not_found"),
            (SettingsError::Storage("disk".into()), "storage"),
            (SettingsError::InvalidSecret("bad"), "invalid"),
            (SettingsError::SecretFromEnv { name: "x", env_var: "Y" }, "invalid"),
            (SettingsError::AdminOnly { field: "llm.base_url".into(), env_var: "Y" }, "invalid"),
        ];
        for (err, kind) in cases {
            let mapped = policy_error(err);
            let actual = match mapped {
                PolicyError::NotFound(_) => "not_found",
                PolicyError::Storage(_) => "storage",
                PolicyError::Invalid(_) => "invalid",
                _ => "other",
            };
            assert_eq!(actual, kind);
        }
    }
}
