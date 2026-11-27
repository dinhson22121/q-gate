use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use super::paths::AppPaths;
use super::templates::{CONFIG_TEMPLATE, SERVER_CONFIG_TEMPLATE};
use crate::settings::domain::config::Config;
use crate::settings::domain::server::ServerConfig;
use crate::settings::domain::validation::ValidationError;

pub const CONFIG_FILE_NAME: &str = ".qgate.toml";

const SERVER_ONLY_KEYS: [(&str, &str); 6] = [
    ("llm", "claude_bin"),
    ("llm", "timeout_secs"),
    ("jira", "base_url"),
    ("jira", "email"),
    ("jira", "provider"),
    ("jira", "mcp"),
];
const SERVER_ONLY_MESSAGE: &str = "can only be set in the server config (qgate.toml in the config dir) for security: \
     a repository's .qgate.toml must not choose where credentials are sent; remove it from this file";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid TOML in {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("invalid configuration in {}: {source}", path.display())]
    Invalid { path: PathBuf, source: ValidationError },
    #[error("cannot write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
}

pub fn load_config(repo_root: &Path) -> Result<Config, ConfigError> {
    let path = repo_root.join(CONFIG_FILE_NAME);
    match read_text(&path)? {
        Some(text) => parse_config(&text, &path.to_string_lossy()),
        None => Ok(Config::default()),
    }
}

pub fn parse_config(text: &str, origin: &str) -> Result<Config, ConfigError> {
    let path = PathBuf::from(origin);
    let table: toml::Table =
        toml::from_str(text).map_err(|e| ConfigError::Parse { path: path.clone(), message: e.to_string() })?;
    if let Some(source) = find_server_only_key(&table) {
        return Err(ConfigError::Invalid { path, source });
    }
    let config: Config =
        toml::from_str(text).map_err(|e| ConfigError::Parse { path: path.clone(), message: e.to_string() })?;
    config.validate().map_err(|source| ConfigError::Invalid { path, source })?;
    Ok(config)
}

fn find_server_only_key(table: &toml::Table) -> Option<ValidationError> {
    SERVER_ONLY_KEYS
        .iter()
        .find(|(section, key)| {
            table.get(*section).and_then(toml::Value::as_table).is_some_and(|section| section.contains_key(*key))
        })
        .map(|(section, key)| ValidationError::new(format!("{section}.{key}"), SERVER_ONLY_MESSAGE))
}

pub fn write_template(repo_root: &Path) -> Result<bool, ConfigError> {
    write_new(&repo_root.join(CONFIG_FILE_NAME), CONFIG_TEMPLATE)
}

pub fn load_server_config(paths: &AppPaths, env: impl Fn(&str) -> Option<String>) -> Result<ServerConfig, ConfigError> {
    let path = paths.server_config_file();
    let config = read_toml::<ServerConfig>(&path)?.with_env_overrides(env);
    config.validate().map_err(|source| ConfigError::Invalid { path, source })?;
    Ok(config)
}

pub fn write_server_template(paths: &AppPaths) -> Result<bool, ConfigError> {
    let path = paths.server_config_file();
    std::fs::create_dir_all(&paths.config_dir)
        .map_err(|source| ConfigError::Write { path: paths.config_dir.clone(), source })?;
    write_new(&path, SERVER_CONFIG_TEMPLATE)
}

pub(super) fn read_toml<T: DeserializeOwned + Default>(path: &Path) -> Result<T, ConfigError> {
    let Some(text) = read_text(path)? else {
        return Ok(T::default());
    };
    toml::from_str(&text).map_err(|e| ConfigError::Parse { path: path.to_path_buf(), message: e.to_string() })
}

fn read_text(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read { path: path.to_path_buf(), source }),
    }
}

fn write_new(path: &Path, content: &str) -> Result<bool, ConfigError> {
    let write_error = |source| ConfigError::Write { path: path.to_path_buf(), source };
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(source) => return Err(write_error(source)),
    };
    file.write_all(content.as_bytes()).map_err(write_error)?;
    file.sync_all().map_err(write_error)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::domain::config::Effort;
    use crate::settings::domain::server::ENV_BIND;

    fn uncomment_examples(template: &str) -> String {
        template
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if rest.starts_with('[') || is_assignment(rest) => rest,
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_assignment(line: &str) -> bool {
        line.split_once(" = ").is_some_and(|(key, _)| key.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
    }

    #[test]
    fn template_parses_validates_and_matches_defaults() {
        let config: Config = toml::from_str(CONFIG_TEMPLATE).unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config, Config::default());
    }

    #[test]
    fn template_examples_parse_and_validate_when_uncommented() {
        let config: Config = toml::from_str(&uncomment_examples(CONFIG_TEMPLATE)).unwrap();
        config.validate().unwrap();
        assert_eq!(config.checks.len(), 1);
        assert_eq!(config.check_specs()[0].command, vec!["go", "test", "./..."]);
        assert_eq!(config.jira.enabled, Some(false));
        assert_eq!(config.jira.key_pattern_projects, Some(vec![]));
    }

    #[test]
    fn server_template_examples_parse_and_validate_when_uncommented() {
        let config: ServerConfig = toml::from_str(&uncomment_examples(SERVER_CONFIG_TEMPLATE)).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn repo_config_cannot_set_server_only_keys() {
        let cases = [
            ("[llm]\nclaude_bin = \"/tmp/evil\"\n", "llm.claude_bin"),
            ("[jira]\nbase_url = \"https://evil.tld\"\n", "jira.base_url"),
            ("[jira]\nprovider = \"mcp\"\n", "jira.provider"),
        ];
        for (text, field) in cases {
            let err = parse_config(text, "abc1234:.qgate.toml").unwrap_err();
            let ConfigError::Invalid { source, .. } = &err else { panic!("{text}: expected Invalid, got {err}") };
            assert_eq!(source.field, field);
            let message = err.to_string();
            assert!(message.contains("abc1234:.qgate.toml"), "{message}");
            assert!(message.contains("server config") && message.contains("security"), "{message}");
        }
    }

    #[test]
    fn parse_config_reports_origin_and_accepts_repo_keys() {
        let config =
            parse_config("[llm]\nmodel = \"claude-m-1\"\n[jira]\nenabled = true\n", "abc:.qgate.toml").unwrap();
        assert_eq!(config.llm.model, "claude-m-1");
        assert_eq!(config.jira.enabled, Some(true));
        let err = parse_config("not toml [", "abc:.qgate.toml").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }) && err.to_string().contains("abc:.qgate.toml"));
        let err = parse_config("[gate]\nnope = 1\n", "abc:.qgate.toml").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }) && err.to_string().contains("nope"));
    }

    #[test]
    fn server_config_accepts_cli_settings() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_dirs(dir.path(), dir.path());
        std::fs::write(paths.server_config_file(), "[llm]\nclaude_bin = \"/opt/claude\"\ntimeout_secs = 900\n")
            .unwrap();
        let llm = load_server_config(&paths, |_| None).unwrap().llm;
        assert_eq!((llm.claude_bin.as_str(), llm.timeout_secs), ("/opt/claude", 900));
        std::fs::write(paths.server_config_file(), "[llm]\ntimeout_secs = 0\n").unwrap();
        assert!(matches!(load_server_config(&paths, |_| None), Err(ConfigError::Invalid { .. })));
    }

    #[test]
    fn server_template_parses_validates_and_matches_defaults() {
        let config: ServerConfig = toml::from_str(SERVER_CONFIG_TEMPLATE).unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config, ServerConfig::default());
    }

    #[test]
    fn docker_example_parses_and_matches_defaults() {
        let config: ServerConfig = toml::from_str(include_str!("../../../config/qgate.toml.example")).unwrap();
        assert_eq!(config, ServerConfig::default());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_config(dir.path()).unwrap(), Config::default());
    }

    #[test]
    fn loads_partial_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "[llm]\neffort = \"max\"\n").unwrap();
        let config = load_config(dir.path()).unwrap();
        assert_eq!(config.llm.effort, Effort::Max);
        assert_eq!(config.llm.concurrency, 4);
    }

    #[test]
    fn parse_errors_name_the_file_and_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "[llm]\nmodle = \"x\"\n").unwrap();
        let err = load_config(dir.path()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        let message = err.to_string();
        assert!(message.contains(CONFIG_FILE_NAME), "{message}");
        assert!(message.contains("modle"), "{message}");
    }

    #[test]
    fn invalid_values_name_the_file_and_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "[llm]\nconcurrency = 99\n").unwrap();
        let message = load_config(dir.path()).unwrap_err().to_string();
        assert!(message.contains(CONFIG_FILE_NAME) && message.contains("llm.concurrency"), "{message}");
    }

    #[test]
    fn unreadable_path_is_a_read_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(CONFIG_FILE_NAME)).unwrap();
        assert!(matches!(load_config(dir.path()), Err(ConfigError::Read { .. })));
    }

    #[test]
    fn write_template_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_template(dir.path()).unwrap());
        assert_eq!(load_config(dir.path()).unwrap(), Config::default());
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "# mine\n").unwrap();
        assert!(!write_template(dir.path()).unwrap());
        assert_eq!(std::fs::read_to_string(dir.path().join(CONFIG_FILE_NAME)).unwrap(), "# mine\n");
    }

    #[test]
    fn write_template_into_missing_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_template(&dir.path().join("nope")).unwrap_err();
        assert!(matches!(err, ConfigError::Write { .. }));
    }

    #[test]
    fn server_config_round_trip_with_env_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_dirs(dir.path().join("config"), dir.path().join("data"));
        assert_eq!(load_server_config(&paths, |_| None).unwrap(), ServerConfig::default());
        assert!(write_server_template(&paths).unwrap());
        assert!(!write_server_template(&paths).unwrap());
        let env = |name: &str| (name == ENV_BIND).then(|| "0.0.0.0:8080".to_string());
        let config = load_server_config(&paths, env).unwrap();
        assert_eq!(config.dashboard.bind, "0.0.0.0:8080");
    }

    #[test]
    fn invalid_env_override_is_reported_against_the_server_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_dirs(dir.path(), dir.path());
        let env = |name: &str| (name == ENV_BIND).then(|| "nope".to_string());
        let err = load_server_config(&paths, env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
        assert!(err.to_string().contains("dashboard.bind"));
    }
}
