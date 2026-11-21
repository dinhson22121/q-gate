//! User-level directories for qgate's own configuration and data.

use std::path::{Path, PathBuf};

const APP_DIR: &str = "qgate";
/// Overrides used in the container image (`/config`, `/data`).
pub const ENV_CONFIG_DIR: &str = "QGATE_CONFIG_DIR";
pub const ENV_DATA_DIR: &str = "QGATE_DATA_DIR";
const SERVER_CONFIG_FILE: &str = "qgate.toml";
const CREDENTIALS_FILE: &str = "credentials.toml";

#[derive(Debug, thiserror::Error)]
#[error("cannot determine the user's {kind} directory; set {var}")]
pub struct AppPathsError {
    kind: &'static str,
    var: &'static str,
}

/// Where qgate keeps user-level files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    /// `QGATE_CONFIG_DIR` / `QGATE_DATA_DIR` when set, else `<os config dir>/qgate` and `<os data dir>/qgate`.
    pub fn discover() -> Result<Self, AppPathsError> {
        Self::resolve(|name| std::env::var(name).ok(), dirs::config_dir(), dirs::data_dir())
    }

    /// Explicit directories (tests, `--config-dir` style flags). Used as-is, no `qgate` suffix.
    pub fn with_dirs(config_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self { config_dir: config_dir.into(), data_dir: data_dir.into() }
    }

    fn resolve(
        env: impl Fn(&str) -> Option<String>,
        system_config: Option<PathBuf>,
        system_data: Option<PathBuf>,
    ) -> Result<Self, AppPathsError> {
        let config_dir =
            pick(&env, ENV_CONFIG_DIR, system_config).ok_or(AppPathsError { kind: "config", var: ENV_CONFIG_DIR })?;
        let data_dir =
            pick(&env, ENV_DATA_DIR, system_data).ok_or(AppPathsError { kind: "data", var: ENV_DATA_DIR })?;
        Ok(Self { config_dir, data_dir })
    }

    /// `<config_dir>/qgate.toml`.
    pub fn server_config_file(&self) -> PathBuf {
        self.config_dir.join(SERVER_CONFIG_FILE)
    }

    /// `<config_dir>/credentials.toml`.
    pub fn credentials_file(&self) -> PathBuf {
        self.config_dir.join(CREDENTIALS_FILE)
    }
}

fn pick(env: &impl Fn(&str) -> Option<String>, var: &str, system: Option<PathBuf>) -> Option<PathBuf> {
    let from_env = env(var).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    from_env.map(PathBuf::from).or_else(|| system.map(|base| app_dir(&base)))
}

fn app_dir(base: &Path) -> PathBuf {
    base.join(APP_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_win_over_system_dirs() {
        let env = |name: &str| match name {
            ENV_CONFIG_DIR => Some("/config".to_string()),
            ENV_DATA_DIR => Some("/data".to_string()),
            _ => None,
        };
        let paths =
            AppPaths::resolve(env, Some("/home/u/.config".into()), Some("/home/u/.local/share".into())).unwrap();
        assert_eq!(paths, AppPaths::with_dirs("/config", "/data"));
        assert_eq!(paths.server_config_file(), PathBuf::from("/config/qgate.toml"));
        assert_eq!(paths.credentials_file(), PathBuf::from("/config/credentials.toml"));
    }

    #[test]
    fn system_dirs_get_an_app_subdirectory() {
        let env = |_: &str| Some(String::new());
        let paths =
            AppPaths::resolve(env, Some("/home/u/.config".into()), Some("/home/u/.local/share".into())).unwrap();
        assert_eq!(paths.config_dir, PathBuf::from("/home/u/.config/qgate"));
        assert_eq!(paths.data_dir, PathBuf::from("/home/u/.local/share/qgate"));
    }

    #[test]
    fn missing_dirs_are_reported_with_the_override_variable() {
        let err = AppPaths::resolve(|_| None, None, Some("/d".into())).unwrap_err();
        assert!(err.to_string().contains(ENV_CONFIG_DIR));
        let err = AppPaths::resolve(|_| None, Some("/c".into()), None).unwrap_err();
        assert!(err.to_string().contains(ENV_DATA_DIR));
    }

    #[test]
    fn discover_does_not_panic() {
        let _ = AppPaths::discover();
    }
}
