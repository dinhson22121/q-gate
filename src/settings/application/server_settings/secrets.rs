use serde::Serialize;

use super::SettingsDeps;
use super::ports::SettingsError;
use crate::settings::application::credentials::{CredentialError, CredentialStore, delete_secret, store_secret};
use crate::settings::domain::CredentialName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretSource {
    Env,
    EnvFile,
    Store,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretStatus {
    pub name: &'static str,
    pub configured: bool,
    pub source: SecretSource,
    pub env_var: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn non_empty(deps: &SettingsDeps, var: &str) -> bool {
    (deps.env)(var).is_some_and(|v| !v.trim().is_empty())
}

pub(super) fn secret_status(deps: &SettingsDeps, name: CredentialName) -> SecretStatus {
    let status =
        |configured, source, env_var| SecretStatus { name: name.as_str(), configured, source, env_var, error: None };
    if non_empty(deps, name.env_var()) {
        return status(true, SecretSource::Env, name.env_var());
    }
    if non_empty(deps, name.file_env_var()) {
        return status(true, SecretSource::EnvFile, name.file_env_var());
    }
    match deps.credentials.get(name) {
        Ok(Some(_)) => status(true, SecretSource::Store, name.env_var()),
        Ok(None) => status(false, SecretSource::Missing, name.env_var()),
        Err(e) => SecretStatus { error: Some(e.to_string()), ..status(false, SecretSource::Missing, name.env_var()) },
    }
}

fn refuse_env_secret(deps: &SettingsDeps, name: CredentialName) -> Result<(), SettingsError> {
    for var in [name.env_var(), name.file_env_var()] {
        if non_empty(deps, var) {
            return Err(SettingsError::SecretFromEnv { name: name.as_str(), env_var: var });
        }
    }
    Ok(())
}

fn map_credential_error(error: CredentialError) -> SettingsError {
    match error {
        CredentialError::InvalidSecret(message) => SettingsError::InvalidSecret(message),
        other => SettingsError::Storage(other.to_string()),
    }
}

pub struct SetSecret<'a> {
    deps: &'a SettingsDeps,
}

impl<'a> SetSecret<'a> {
    pub fn new(deps: &'a SettingsDeps) -> Self {
        Self { deps }
    }

    pub fn execute(&self, name: CredentialName, value: &str) -> Result<SecretStatus, SettingsError> {
        let _guard = self.deps.write_guard();
        refuse_env_secret(self.deps, name)?;
        store_secret(name, value, self.deps.credentials.as_ref() as &dyn CredentialStore)
            .map_err(map_credential_error)?;
        Ok(secret_status(self.deps, name))
    }
}

pub struct ClearSecret<'a> {
    deps: &'a SettingsDeps,
}

impl<'a> ClearSecret<'a> {
    pub fn new(deps: &'a SettingsDeps) -> Self {
        Self { deps }
    }

    pub fn execute(&self, name: CredentialName) -> Result<SecretStatus, SettingsError> {
        let _guard = self.deps.write_guard();
        refuse_env_secret(self.deps, name)?;
        delete_secret(name, self.deps.credentials.as_ref()).map_err(map_credential_error)?;
        Ok(secret_status(self.deps, name))
    }
}
