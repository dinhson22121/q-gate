use serde::Serialize;
use serde_json::Value;

use super::SettingsDeps;
use super::fields::{changed_paths, copy, from_json, get, to_json};
use super::ports::{SettingsError, StoredSettings};
use super::secrets::{SecretStatus, secret_status};
use crate::settings::domain::{
    ADMIN_ONLY_FIELDS, CredentialName, RESTART_REQUIRED_FIELDS, RUN_REPO_CHECKS_FIELD, ServerConfig, ValidationError,
    WEBHOOK_URL_FIELD, active_env_overrides, admin_env_var,
};

pub const PAGE_CHANGED_BY: &str = "settings-page";
const ROLLBACK_PREFIX: &str = "rollback:v";
const WEBHOOK_FIELD: &str = WEBHOOK_URL_FIELD;
const WEBHOOK_INVALID: &str = "must be an http:// or https:// URL with a host (no credentials in the URL)";
const HISTORY_LIMIT_MAX: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockedField {
    pub field: &'static str,
    pub env_var: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingsView {
    pub config: ServerConfig,
    pub locked: Vec<LockedField>,
    pub admin_only: Vec<LockedField>,
    pub restart_required_fields: Vec<&'static str>,
    pub pending_restart: Vec<String>,
    pub secrets: Vec<SecretStatus>,
    pub webhook_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookChange {
    Keep,
    Clear,
    Set(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub version: Option<i64>,
    pub changed_fields: Vec<String>,
    pub restart_required: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    pub version: i64,
    pub changed_by: String,
    pub created_at: String,
    pub changed_fields: Vec<String>,
}

fn locked_fields(deps: &SettingsDeps) -> Vec<LockedField> {
    active_env_overrides(&|name: &str| (deps.env)(name))
        .into_iter()
        .map(|(entry, _)| LockedField { field: entry.field, env_var: entry.env_var })
        .collect()
}

fn admin_only_fields() -> Vec<LockedField> {
    ADMIN_ONLY_FIELDS
        .into_iter()
        .chain([WEBHOOK_FIELD, RUN_REPO_CHECKS_FIELD])
        .filter_map(|field| admin_env_var(field).map(|env_var| LockedField { field, env_var }))
        .collect()
}

fn admin_only(field: &str) -> SettingsError {
    let env_var = admin_env_var(field).unwrap_or("its environment variable");
    SettingsError::AdminOnly { field: field.to_string(), env_var }
}

fn reject_admin_only_changes(current: &Value, next: &Value, webhook: &WebhookChange) -> Result<(), SettingsError> {
    let enables_checks = get(next, RUN_REPO_CHECKS_FIELD) == Some(&Value::Bool(true))
        && get(current, RUN_REPO_CHECKS_FIELD) != Some(&Value::Bool(true));
    let field = ADMIN_ONLY_FIELDS
        .into_iter()
        .find(|field| get(next, field) != get(current, field))
        .or_else(|| matches!(webhook, WebhookChange::Set(_)).then_some(WEBHOOK_FIELD))
        .or_else(|| enables_checks.then_some(RUN_REPO_CHECKS_FIELD));
    field.map_or(Ok(()), |field| Err(admin_only(field)))
}

fn pin_admin_only(current: &ServerConfig, target: ServerConfig) -> Result<ServerConfig, SettingsError> {
    let keep_webhook = !target.notify.webhook_url.is_empty();
    let keep_checks = target.watch.run_repo_checks && !current.watch.run_repo_checks;
    let current = to_json(current)?;
    let mut next = to_json(&target)?;
    let pinned = ADMIN_ONLY_FIELDS
        .into_iter()
        .chain(keep_webhook.then_some(WEBHOOK_FIELD))
        .chain(keep_checks.then_some(RUN_REPO_CHECKS_FIELD));
    for field in pinned {
        copy(&current, &mut next, field);
    }
    from_json(next)
}

fn pending_restart(deps: &SettingsDeps, effective: &ServerConfig) -> Result<Vec<String>, SettingsError> {
    let (boot, now) = (to_json(&deps.boot)?, to_json(effective)?);
    Ok(RESTART_REQUIRED_FIELDS.iter().filter(|f| get(&boot, f) != get(&now, f)).map(|f| f.to_string()).collect())
}

fn current_raw(deps: &SettingsDeps) -> Result<ServerConfig, SettingsError> {
    Ok(deps.store.load_raw()?.unwrap_or_default())
}

fn validate(effective: &ServerConfig) -> Result<(), SettingsError> {
    effective.validate().map_err(|e| {
        if e.field == WEBHOOK_FIELD {
            SettingsError::Invalid(ValidationError::new(WEBHOOK_FIELD, WEBHOOK_INVALID))
        } else {
            SettingsError::Invalid(e)
        }
    })
}

fn commit(
    deps: &SettingsDeps,
    previous: &ServerConfig,
    raw: ServerConfig,
    changed_by: &str,
    force: bool,
) -> Result<Applied, SettingsError> {
    let effective = deps.effective(raw.clone());
    validate(&effective)?;
    let changed_fields = changed_paths(&to_json(previous)?, &to_json(&raw)?);
    let version = if force || !changed_fields.is_empty() { Some(deps.store.save_raw(&raw, changed_by)?) } else { None };
    let restart_required = pending_restart(deps, &effective)?;
    deps.replace_shared(effective);
    Ok(Applied { version, changed_fields, restart_required })
}

pub struct GetSettings<'a> {
    deps: &'a SettingsDeps,
}

impl<'a> GetSettings<'a> {
    pub fn new(deps: &'a SettingsDeps) -> Self {
        Self { deps }
    }

    pub fn execute(&self) -> Result<SettingsView, SettingsError> {
        let mut config = self.deps.effective(current_raw(self.deps)?);
        let pending_restart = pending_restart(self.deps, &config)?;
        let webhook_configured = !config.notify.webhook_url.trim().is_empty();
        config.notify.webhook_url = String::new();
        Ok(SettingsView {
            config,
            locked: locked_fields(self.deps),
            admin_only: admin_only_fields(),
            restart_required_fields: RESTART_REQUIRED_FIELDS.to_vec(),
            pending_restart,
            secrets: CredentialName::ALL.into_iter().map(|name| secret_status(self.deps, name)).collect(),
            webhook_configured,
        })
    }
}

pub struct UpdateSettings<'a> {
    deps: &'a SettingsDeps,
}

impl<'a> UpdateSettings<'a> {
    pub fn new(deps: &'a SettingsDeps) -> Self {
        Self { deps }
    }

    pub fn execute(&self, patch: ServerConfig, webhook: WebhookChange) -> Result<Applied, SettingsError> {
        let _guard = self.deps.write_guard();
        self.save(patch, webhook, PAGE_CHANGED_BY)
    }

    pub fn execute_as(
        &self,
        patch: ServerConfig,
        webhook: WebhookChange,
        changed_by: &str,
        expected_version: i64,
    ) -> Result<Applied, SettingsError> {
        let _guard = self.deps.write_guard();
        let latest = latest_version(self.deps)?;
        if latest != expected_version {
            return Err(SettingsError::Conflict(format!(
                "the settings changed (version {expected_version} → {latest}); reload and try again"
            )));
        }
        self.save(patch, webhook, changed_by)
    }

    fn save(&self, patch: ServerConfig, webhook: WebhookChange, changed_by: &str) -> Result<Applied, SettingsError> {
        let raw = current_raw(self.deps)?;
        let current = to_json(&self.deps.effective(raw.clone()))?;
        let raw_json = to_json(&raw)?;
        let mut next = to_json(&patch)?;
        reject_admin_only_changes(&current, &next, &webhook)?;
        for (entry, _) in active_env_overrides(&|name: &str| (self.deps.env)(name)) {
            let changed = if entry.field == WEBHOOK_FIELD {
                webhook != WebhookChange::Keep
            } else {
                get(&next, entry.field) != get(&current, entry.field)
            };
            if changed {
                return Err(SettingsError::Locked { field: entry.field.to_string(), env_var: entry.env_var });
            }
            copy(&raw_json, &mut next, entry.field);
        }
        for field in ADMIN_ONLY_FIELDS {
            copy(&raw_json, &mut next, field);
        }
        let mut next = from_json(next)?;
        next.notify.webhook_url = match webhook {
            WebhookChange::Keep => raw.notify.webhook_url.clone(),
            WebhookChange::Clear => String::new(),
            WebhookChange::Set(url) => url.trim().to_string(),
        };
        commit(self.deps, &raw, next, changed_by, false)
    }
}

pub fn latest_version(deps: &SettingsDeps) -> Result<i64, SettingsError> {
    Ok(deps.store.recent(1)?.first().map_or(0, |row| row.version))
}

pub struct SettingsHistory<'a> {
    deps: &'a SettingsDeps,
}

impl<'a> SettingsHistory<'a> {
    pub fn new(deps: &'a SettingsDeps) -> Self {
        Self { deps }
    }

    pub fn execute(&self, limit: u32) -> Result<Vec<HistoryEntry>, SettingsError> {
        let limit = limit.clamp(1, HISTORY_LIMIT_MAX);
        let rows = self.deps.store.recent(limit + 1)?;
        let defaults = StoredSettings {
            version: 0,
            changed_by: String::new(),
            created_at: String::new(),
            config: ServerConfig::default(),
        };
        rows.iter()
            .take(limit as usize)
            .enumerate()
            .map(|(index, row)| {
                let previous = rows.get(index + 1).unwrap_or(&defaults);
                Ok(HistoryEntry {
                    version: row.version,
                    changed_by: row.changed_by.clone(),
                    created_at: row.created_at.clone(),
                    changed_fields: changed_paths(&to_json(&previous.config)?, &to_json(&row.config)?),
                })
            })
            .collect()
    }
}

pub struct RollbackSettings<'a> {
    deps: &'a SettingsDeps,
}

impl<'a> RollbackSettings<'a> {
    pub fn new(deps: &'a SettingsDeps) -> Self {
        Self { deps }
    }

    pub fn execute(&self, version: i64) -> Result<Applied, SettingsError> {
        let _guard = self.deps.write_guard();
        let target = self.deps.store.version(version)?.ok_or(SettingsError::VersionNotFound(version))?;
        let raw = current_raw(self.deps)?;
        let config = pin_admin_only(&raw, target.config)?;
        commit(self.deps, &raw, config, &format!("{ROLLBACK_PREFIX}{version}"), true)
    }
}
