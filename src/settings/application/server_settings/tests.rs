use std::sync::{Arc, RwLock};

use super::fakes::{FixedSeed, MemoryConfigStore, env};
use super::*;
use crate::settings::application::credentials::fakes::MemoryStore;
use crate::settings::domain::{CredentialName, ServerConfig};

const WEBHOOK: &str = "https://hooks.slack.com/services/T0/B0/secret-token";

struct Fixture {
    store: Arc<MemoryConfigStore>,
    credentials: Arc<MemoryStore>,
    shared: SharedServerConfig,
    deps: SettingsDeps,
}

fn fixture_with(raw: ServerConfig, pairs: &[(&str, &str)]) -> Fixture {
    let store = Arc::new(MemoryConfigStore::default());
    store.save_raw(&raw, "test").unwrap();
    let environment = env(pairs);
    let effective =
        LoadEffectiveSettings::new(store.clone(), Arc::new(FixedSeed(None)), environment.clone()).execute().unwrap();
    let shared: SharedServerConfig = Arc::new(RwLock::new(effective));
    let credentials = Arc::new(MemoryStore::default());
    let deps = SettingsDeps::new(store.clone(), credentials.clone(), shared.clone(), environment);
    Fixture { store, credentials, shared, deps }
}

fn fixture(pairs: &[(&str, &str)]) -> Fixture {
    fixture_with(ServerConfig::default(), pairs)
}

fn page_patch(f: &Fixture) -> ServerConfig {
    GetSettings::new(&f.deps).execute().unwrap().config
}

fn latest(f: &Fixture) -> ServerConfig {
    f.store.load_raw().unwrap().unwrap()
}

#[test]
fn load_imports_seed_once_then_ignores_it() {
    let store = Arc::new(MemoryConfigStore::default());
    let mut seeded = ServerConfig::default();
    seeded.repos.scan_depth = 5;
    let load = LoadEffectiveSettings::new(store.clone(), Arc::new(FixedSeed(Some(seeded))), env(&[]));
    assert_eq!(load.execute().unwrap().repos.scan_depth, 5);
    assert_eq!(store.rows.lock().unwrap()[0].changed_by, "import:qgate.toml");

    let mut changed = ServerConfig::default();
    changed.repos.scan_depth = 7;
    store.save_raw(&changed, PAGE_CHANGED_BY).unwrap();
    assert_eq!(load.execute().unwrap().repos.scan_depth, 7, "the file is not read again");
    assert_eq!(store.rows.lock().unwrap().len(), 2);
}

#[test]
fn load_without_file_stores_defaults_and_applies_env() {
    let store = Arc::new(MemoryConfigStore::default());
    let load =
        LoadEffectiveSettings::new(store.clone(), Arc::new(FixedSeed(None)), env(&[("QGATE_BIND", "0.0.0.0:9")]));
    assert_eq!(load.execute().unwrap().dashboard.bind, "0.0.0.0:9");
    let rows = store.rows.lock().unwrap();
    assert_eq!(rows[0].changed_by, DEFAULTS_ORIGIN);
    assert_eq!(rows[0].config, ServerConfig::default(), "env values are not persisted");
}

#[test]
fn load_rejects_invalid_seed_and_invalid_env() {
    let mut bad = ServerConfig::default();
    bad.repos.scan_depth = 0;
    let store = Arc::new(MemoryConfigStore::default());
    let err =
        LoadEffectiveSettings::new(store.clone(), Arc::new(FixedSeed(Some(bad))), env(&[])).execute().unwrap_err();
    assert!(err.to_string().contains("import:qgate.toml") && err.to_string().contains("repos.scan_depth"), "{err}");
    assert!(store.rows.lock().unwrap().is_empty());

    store.save_raw(&ServerConfig::default(), "x").unwrap();
    let err = LoadEffectiveSettings::new(store, Arc::new(FixedSeed(None)), env(&[("QGATE_BIND", "nope")]))
        .execute()
        .unwrap_err();
    assert!(matches!(err, SettingsError::Invalid(e) if e.field == "dashboard.bind"));
}

#[test]
fn view_masks_webhook_and_lists_locks_and_secrets() {
    let mut raw = ServerConfig::default();
    raw.notify.webhook_url = WEBHOOK.into();
    let f = fixture_with(raw, &[("JIRA_EMAIL", "bot@acme.com"), ("FIGMA_TOKEN_FILE", "/run/secrets/figma")]);
    f.credentials.set(CredentialName::JiraApiToken, &crate::settings::domain::Secret::new("tok")).unwrap();
    let view = GetSettings::new(&f.deps).execute().unwrap();
    assert_eq!(view.config.notify.webhook_url, "");
    assert!(view.webhook_configured);
    assert_eq!(view.config.jira.email, "bot@acme.com");
    assert_eq!(view.locked, vec![LockedField { field: "jira.email", env_var: "JIRA_EMAIL" }]);
    assert_eq!(view.admin_only.len(), 13);
    assert!(view.admin_only.contains(&LockedField { field: "llm.claude_bin", env_var: "QGATE_CLAUDE_BIN" }));
    assert!(view.admin_only.contains(&LockedField { field: "watch.base_branch", env_var: "QGATE_WATCH_BASE_BRANCH" }));
    assert_eq!(view.restart_required_fields.len(), 9);
    let sources: Vec<_> = view.secrets.iter().map(|s| (s.name, s.configured, s.source)).collect();
    assert_eq!(
        sources,
        vec![("jira_api_token", true, SecretSource::Store), ("figma_token", true, SecretSource::EnvFile),]
    );
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains("secret-token") && !json.contains("tok\""), "{json}");
}

#[test]
fn store_errors_show_as_missing_with_message() {
    let f = fixture(&[]);
    let deps = SettingsDeps::new(f.store.clone(), Arc::new(MemoryStore::failing()), f.shared.clone(), env(&[]));
    let view = GetSettings::new(&deps).execute().unwrap();
    assert!(view.secrets.iter().all(|s| !s.configured && s.error.is_some()));
}

#[test]
fn update_saves_raw_config_and_replaces_live_config() {
    let f = fixture(&[("JIRA_EMAIL", "bot@acme.com")]);
    let mut patch = page_patch(&f);
    patch.agents.max_parallel = 5;
    patch.jira.enabled = true;
    let applied = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert_eq!(applied.version, Some(2));
    assert_eq!(applied.changed_fields, vec!["agents.max_parallel".to_string(), "jira.enabled".to_string()]);
    assert!(applied.restart_required.is_empty());
    assert_eq!(latest(&f).jira.email, "", "env value is not written");
    let live = f.shared.read().unwrap().clone();
    assert_eq!((live.agents.max_parallel, live.jira.email.as_str()), (5, "bot@acme.com"));
}

#[test]
fn update_as_records_the_author_and_refuses_a_stale_version() {
    let f = fixture(&[]);
    assert_eq!(latest_version(&f.deps).unwrap(), 1);
    let mut patch = page_patch(&f);
    patch.agents.max_parallel = 6;
    let stale = UpdateSettings::new(&f.deps).execute_as(patch.clone(), WebhookChange::Keep, "policy-assistant:p1", 0);
    assert!(matches!(stale, Err(SettingsError::Conflict(m)) if m.contains("version 0 → 1")));
    let applied =
        UpdateSettings::new(&f.deps).execute_as(patch, WebhookChange::Keep, "policy-assistant:p1", 1).unwrap();
    assert_eq!(applied.version, Some(2));
    assert_eq!(f.store.rows.lock().unwrap()[1].changed_by, "policy-assistant:p1");
    assert_eq!(latest_version(&f.deps).unwrap(), 2);
}

#[test]
fn unchanged_save_writes_no_version() {
    let f = fixture(&[]);
    let applied = UpdateSettings::new(&f.deps).execute(page_patch(&f), WebhookChange::Keep).unwrap();
    assert_eq!(applied.version, None);
    assert_eq!(f.store.rows.lock().unwrap().len(), 1);
}

#[test]
fn locked_field_change_is_rejected_naming_the_env_var() {
    let f = fixture(&[("QGATE_BIND", "0.0.0.0:8080"), ("QGATE_WEBHOOK_URL", WEBHOOK)]);
    let mut patch = page_patch(&f);
    patch.dashboard.bind = "127.0.0.1:9999".into();
    let err = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap_err();
    assert_eq!(err, SettingsError::Locked { field: "dashboard.bind".into(), env_var: "QGATE_BIND" });
    assert!(err.to_string().contains("QGATE_BIND"));

    let err = UpdateSettings::new(&f.deps).execute(page_patch(&f), WebhookChange::Clear).unwrap_err();
    assert!(matches!(err, SettingsError::Locked { env_var: "QGATE_WEBHOOK_URL", .. }));
    assert_eq!(f.store.rows.lock().unwrap().len(), 1);
}

#[test]
fn webhook_keep_clear_and_set() {
    let mut raw = ServerConfig::default();
    raw.notify.webhook_url = WEBHOOK.into();
    let f = fixture_with(raw, &[]);
    let mut patch = page_patch(&f);
    patch.watch.run_repo_checks = false;
    UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert_eq!(latest(&f).notify.webhook_url, WEBHOOK);

    let err = UpdateSettings::new(&f.deps)
        .execute(page_patch(&f), WebhookChange::Set(" https://ntfy.sh/x ".into()))
        .unwrap_err();
    assert_eq!(err, SettingsError::AdminOnly { field: "notify.webhook_url".into(), env_var: "QGATE_WEBHOOK_URL" });
    assert_eq!(latest(&f).notify.webhook_url, WEBHOOK);

    UpdateSettings::new(&f.deps).execute(page_patch(&f), WebhookChange::Clear).unwrap();
    assert_eq!(latest(&f).notify.webhook_url, "");
    assert!(!GetSettings::new(&f.deps).execute().unwrap().webhook_configured);
}

#[test]
fn invalid_values_are_rejected_without_echoing_the_webhook() {
    let f = fixture(&[]);
    let mut patch = page_patch(&f);
    patch.figma.block_on = vec![crate::settings::domain::DesignCheck::States];
    let err = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap_err();
    assert!(matches!(err, SettingsError::Invalid(ref e) if e.field == "figma.block_on"));

    let err =
        UpdateSettings::new(&f.deps).execute(page_patch(&f), WebhookChange::Set("nope-token".into())).unwrap_err();
    assert!(matches!(err, SettingsError::AdminOnly { ref field, .. } if field == "notify.webhook_url"));
    assert!(!err.to_string().contains("nope-token"));
    let mut raw = ServerConfig::default();
    raw.notify.webhook_url = "nope-token".into();
    f.store.save_raw(&raw, "legacy").unwrap();
    let err = UpdateSettings::new(&f.deps).execute(page_patch(&f), WebhookChange::Keep).unwrap_err();
    assert!(matches!(err, SettingsError::Invalid(ref e) if e.field == "notify.webhook_url"));
    assert!(!err.to_string().contains("nope-token"));
    assert_eq!(f.store.rows.lock().unwrap().len(), 2);
}

#[test]
fn restart_list_tracks_fields_that_differ_from_boot() {
    let f = fixture(&[]);
    let mut patch = page_patch(&f);
    patch.watch.poll_interval_secs = 9;
    patch.dashboard.allowed_hosts.push("qgate.local".into());
    let applied = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert_eq!(
        applied.restart_required,
        vec!["dashboard.allowed_hosts".to_string(), "watch.poll_interval_secs".into()]
    );
    let mut patch = page_patch(&f);
    patch.agents.enabled = false;
    let applied = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert_eq!(applied.restart_required.len(), 2);
    assert_eq!(GetSettings::new(&f.deps).execute().unwrap().pending_restart.len(), 2);
}

#[test]
fn history_lists_changed_paths_and_rollback_appends_a_version() {
    let mut raw = ServerConfig::default();
    raw.notify.webhook_url = WEBHOOK.into();
    let f = fixture_with(raw, &[]);
    let mut patch = page_patch(&f);
    patch.agents.max_parallel = 4;
    UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Clear).unwrap();

    let history = SettingsHistory::new(&f.deps).execute(20).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version, 2);
    assert_eq!(history[0].changed_by, PAGE_CHANGED_BY);
    assert_eq!(history[0].changed_fields, vec!["agents.max_parallel".to_string(), "notify.webhook_url".into()]);
    assert_eq!(history[1].changed_fields, vec!["notify.webhook_url".to_string()], "oldest diffs against defaults");
    assert!(!serde_json::to_string(&history).unwrap().contains("secret-token"));
    assert_eq!(SettingsHistory::new(&f.deps).execute(1).unwrap().len(), 1);

    let applied = RollbackSettings::new(&f.deps).execute(1).unwrap();
    assert_eq!(applied.version, Some(3));
    assert_eq!(latest(&f).agents.max_parallel, 3);
    assert_eq!(latest(&f).notify.webhook_url, "", "a rollback never restores a webhook");
    assert_eq!(f.store.rows.lock().unwrap()[2].changed_by, "rollback:v1");
    assert_eq!(f.shared.read().unwrap().agents.max_parallel, 3);
    assert_eq!(RollbackSettings::new(&f.deps).execute(42).unwrap_err(), SettingsError::VersionNotFound(42));
}

#[test]
fn secrets_are_set_and_cleared_but_not_when_from_env() {
    let f = fixture(&[("JIRA_API_TOKEN", "sk-env")]);
    let status = SetSecret::new(&f.deps).execute(CredentialName::FigmaToken, " figd_123 ").unwrap();
    assert_eq!((status.configured, status.source), (true, SecretSource::Store));
    assert_eq!(f.credentials.get(CredentialName::FigmaToken).unwrap().unwrap().expose(), "figd_123");
    let status = ClearSecret::new(&f.deps).execute(CredentialName::FigmaToken).unwrap();
    assert_eq!(status.source, SecretSource::Missing);

    let err = SetSecret::new(&f.deps).execute(CredentialName::JiraApiToken, "sk-new").unwrap_err();
    assert_eq!(err, SettingsError::SecretFromEnv { name: "jira_api_token", env_var: "JIRA_API_TOKEN" });
    assert!(ClearSecret::new(&f.deps).execute(CredentialName::JiraApiToken).is_err());
    let err = SetSecret::new(&f.deps).execute(CredentialName::FigmaToken, "a b").unwrap_err();
    assert!(matches!(err, SettingsError::InvalidSecret(_)));
    assert!(!err.to_string().contains("a b"));
}

#[test]
fn credential_store_failures_are_storage_errors() {
    let f = fixture(&[]);
    let deps = SettingsDeps::new(f.store.clone(), Arc::new(MemoryStore::failing()), f.shared.clone(), env(&[]));
    assert!(matches!(SetSecret::new(&deps).execute(CredentialName::JiraApiToken, "x"), Err(SettingsError::Storage(_))));
}

#[test]
fn output_dir_is_env_locked_and_restart_only_while_the_rest_is_live() {
    let f = fixture(&[("QGATE_OUTPUT_DIR", "/data/reviews")]);
    let view = GetSettings::new(&f.deps).execute().unwrap();
    assert_eq!(view.config.output.dir, "/data/reviews");
    assert!(view.locked.contains(&LockedField { field: "output.dir", env_var: "QGATE_OUTPUT_DIR" }));

    let mut patch = page_patch(&f);
    patch.output.keep_last = 5;
    patch.output.include_passing = false;
    let applied = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert_eq!(applied.changed_fields, vec!["output.include_passing".to_string(), "output.keep_last".into()]);
    assert!(applied.restart_required.is_empty());
    assert_eq!(f.shared.read().unwrap().output.keep_last, 5);

    let mut patch = page_patch(&f);
    patch.output.dir = "/elsewhere".into();
    let err = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap_err();
    assert_eq!(err, SettingsError::Locked { field: "output.dir".into(), env_var: "QGATE_OUTPUT_DIR" });

    let unlocked = fixture(&[]);
    let mut patch = page_patch(&unlocked);
    patch.output.dir = "/elsewhere".into();
    let applied = UpdateSettings::new(&unlocked.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert_eq!(applied.restart_required, vec!["output.dir".to_string()]);
}

#[test]
fn admin_only_fields_cannot_change_over_http() {
    let f = fixture(&[("QGATE_CLAUDE_BIN", "/opt/claude/bin/claude")]);
    let cases: [fn(&mut ServerConfig); 6] = [
        |c| c.llm.claude_bin = "/tmp/evil".into(),
        |c| c.jira.base_url = "https://attacker.example".into(),
        |c| c.jira.mcp.command = vec!["sh".into()],
        |c| c.jira.mcp.env_passthrough.push("ANTHROPIC_API_KEY".into()),
        |c| c.figma.api_base_url = "https://attacker.example".into(),
        |c| c.figma.mcp_url = "https://attacker.example/mcp".into(),
    ];
    for change in cases {
        let mut patch = page_patch(&f);
        change(&mut patch);
        let err = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap_err();
        assert!(matches!(err, SettingsError::AdminOnly { .. }), "{err:?}");
        assert!(err.to_string().contains("admin-only"));
    }
    let err = UpdateSettings::new(&f.deps)
        .execute_as(page_patch(&f), WebhookChange::Set("https://x.example".into()), "policy-assistant:p1", 1)
        .unwrap_err();
    assert!(matches!(err, SettingsError::AdminOnly { .. }));
    assert_eq!(f.store.rows.lock().unwrap().len(), 1, "nothing was saved");

    let mut patch = page_patch(&f);
    patch.agents.max_parallel = 4;
    UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert_eq!(latest(&f).llm.claude_bin, "claude");
    assert_eq!(f.shared.read().unwrap().llm.claude_bin, "/opt/claude/bin/claude");
}

#[test]
fn repository_checks_can_be_turned_off_but_not_on() {
    let f = fixture(&[]);
    let mut patch = page_patch(&f);
    patch.watch.run_repo_checks = false;
    UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap();
    assert!(!latest(&f).watch.run_repo_checks);

    let mut patch = page_patch(&f);
    patch.watch.run_repo_checks = true;
    let err = UpdateSettings::new(&f.deps).execute(patch, WebhookChange::Keep).unwrap_err();
    assert_eq!(
        err,
        SettingsError::AdminOnly { field: "watch.run_repo_checks".into(), env_var: "QGATE_RUN_REPO_CHECKS" }
    );

    let env_locked = fixture(&[("QGATE_RUN_REPO_CHECKS", "true")]);
    let mut patch = page_patch(&env_locked);
    patch.watch.run_repo_checks = false;
    let err = UpdateSettings::new(&env_locked.deps).execute(patch, WebhookChange::Keep).unwrap_err();
    assert!(matches!(err, SettingsError::Locked { env_var: "QGATE_RUN_REPO_CHECKS", .. }), "env wins");
}

#[test]
fn rollback_keeps_current_admin_only_values() {
    let mut old = ServerConfig::default();
    old.llm.claude_bin = "/old/claude".into();
    old.jira.mcp.command = vec!["old-mcp".into()];
    old.notify.webhook_url = WEBHOOK.into();
    old.agents.max_parallel = 5;
    let f = fixture_with(old, &[]);
    let mut current = ServerConfig::default();
    current.watch.run_repo_checks = false;
    f.store.save_raw(&current, "admin").unwrap();

    RollbackSettings::new(&f.deps).execute(1).unwrap();
    let rolled = latest(&f);
    assert_eq!(rolled.agents.max_parallel, 5, "ordinary fields are restored");
    assert_eq!(rolled.llm.claude_bin, "claude");
    assert_eq!(rolled.jira.mcp.command, ServerConfig::default().jira.mcp.command);
    assert_eq!(rolled.notify.webhook_url, "");
    assert!(!rolled.watch.run_repo_checks, "a rollback never turns repository checks on");

    let mut tight = ServerConfig::default();
    tight.watch.run_repo_checks = false;
    let g = fixture_with(tight, &[]);
    let mut loose = ServerConfig::default();
    loose.notify.webhook_url = WEBHOOK.into();
    g.store.save_raw(&loose, "admin").unwrap();
    RollbackSettings::new(&g.deps).execute(1).unwrap();
    assert!(!latest(&g).watch.run_repo_checks);
    assert_eq!(latest(&g).notify.webhook_url, "", "the cleared webhook of the target is restored");
}
