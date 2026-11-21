use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::settings::application::{ServerConfigStore, SettingsError, StoredSettings};
use crate::settings::domain::ServerConfig;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const MIGRATIONS: &[&str] = &["
    CREATE TABLE server_settings (
        version     INTEGER PRIMARY KEY AUTOINCREMENT,
        config_json TEXT    NOT NULL,
        changed_by  TEXT,
        created_at  TEXT    NOT NULL
    );
"];

const COLUMNS: &str = "version, config_json, changed_by, created_at";

pub struct SqliteServerConfigStore {
    conn: Mutex<Connection>,
}

fn storage(err: impl std::fmt::Display) -> SettingsError {
    SettingsError::Storage(format!("settings database: {err}"))
}

impl SqliteServerConfigStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(storage)?;
        }
        let mut conn = Connection::open(path).map_err(storage)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)).map_err(storage)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(storage)?;
        migrate(&mut conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, SettingsError> {
        self.conn.lock().map_err(|_| SettingsError::Storage("settings database lock poisoned".into()))
    }
}

fn current_version(conn: &Connection) -> Result<u32, SettingsError> {
    conn.query_row("SELECT COALESCE(MAX(version), 0) FROM settings_schema_version", [], |r| r.get(0)).map_err(storage)
}

fn migrate(conn: &mut Connection) -> Result<(), SettingsError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS settings_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied = current_version(&tx)? as usize;
    if applied > MIGRATIONS.len() {
        return Err(SettingsError::Storage(format!(
            "settings schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO settings_schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn encode(config: &ServerConfig) -> Result<String, SettingsError> {
    serde_json::to_string(config).map_err(storage)
}

fn read_row(row: &Row<'_>) -> rusqlite::Result<(i64, String, Option<String>, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn decode(raw: (i64, String, Option<String>, String)) -> Result<StoredSettings, SettingsError> {
    let (version, json, changed_by, created_at) = raw;
    let config = serde_json::from_str(&json)
        .map_err(|e| SettingsError::Storage(format!("settings version {version} cannot be read: {e}")))?;
    Ok(StoredSettings { version, changed_by: changed_by.unwrap_or_default(), created_at, config })
}

impl ServerConfigStore for SqliteServerConfigStore {
    fn load_raw(&self) -> Result<Option<ServerConfig>, SettingsError> {
        Ok(self.recent(1)?.into_iter().next().map(|s| s.config))
    }

    fn save_raw(&self, config: &ServerConfig, changed_by: &str) -> Result<i64, SettingsError> {
        let json = encode(config)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO server_settings (config_json, changed_by, created_at) VALUES (?1, ?2, ?3)",
            params![json, changed_by, now()],
        )
        .map_err(storage)?;
        Ok(conn.last_insert_rowid())
    }

    fn seed(&self, config: &ServerConfig, changed_by: &str) -> Result<Option<i64>, SettingsError> {
        let json = encode(config)?;
        let conn = self.lock()?;
        let inserted = conn
            .execute(
                "INSERT INTO server_settings (config_json, changed_by, created_at) \
                 SELECT ?1, ?2, ?3 WHERE NOT EXISTS (SELECT 1 FROM server_settings)",
                params![json, changed_by, now()],
            )
            .map_err(storage)?;
        Ok((inserted > 0).then(|| conn.last_insert_rowid()))
    }

    fn version(&self, version: i64) -> Result<Option<StoredSettings>, SettingsError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(&format!("SELECT {COLUMNS} FROM server_settings WHERE version = ?1"), [version], read_row)
            .optional()
            .map_err(storage)?;
        row.map(decode).transpose()
    }

    fn recent(&self, limit: u32) -> Result<Vec<StoredSettings>, SettingsError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM server_settings ORDER BY version DESC LIMIT ?1"))
            .map_err(storage)?;
        let rows = stmt.query_map([limit], read_row).map_err(storage)?;
        rows.map(|r| r.map_err(storage).and_then(decode)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (tempfile::TempDir, SqliteServerConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteServerConfigStore::open(dir.path().join("data").join("qgate.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn empty_store_has_no_settings() {
        let (_dir, store) = open();
        assert_eq!(store.load_raw().unwrap(), None);
        assert!(store.recent(5).unwrap().is_empty());
        assert_eq!(store.version(1).unwrap(), None);
    }

    #[test]
    fn round_trip_and_append_only_versions() {
        let (_dir, store) = open();
        let mut config = ServerConfig::default();
        config.agents.max_parallel = 5;
        config.figma.block_on = vec![crate::settings::domain::DesignCheck::Token];
        assert_eq!(store.save_raw(&ServerConfig::default(), "a").unwrap(), 1);
        assert_eq!(store.save_raw(&config, "b").unwrap(), 2);
        assert_eq!(store.load_raw().unwrap(), Some(config.clone()));
        let recent = store.recent(10).unwrap();
        assert_eq!(recent.iter().map(|s| s.version).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(recent[0].changed_by, "b");
        assert!(recent[0].created_at.ends_with('Z'));
        assert_eq!(store.version(1).unwrap().unwrap().config, ServerConfig::default());
        assert_eq!(store.recent(1).unwrap().len(), 1);
    }

    #[test]
    fn seed_only_inserts_into_an_empty_table() {
        let (_dir, store) = open();
        assert_eq!(store.seed(&ServerConfig::default(), "import:qgate.toml").unwrap(), Some(1));
        assert_eq!(store.seed(&ServerConfig::default(), "import:qgate.toml").unwrap(), None);
        assert_eq!(store.recent(10).unwrap().len(), 1);
    }

    #[test]
    fn reopening_keeps_data_and_coexists_with_history_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let history = crate::history::infrastructure::SqliteRunRepository::open(&path).unwrap();
        SqliteServerConfigStore::open(&path).unwrap().save_raw(&ServerConfig::default(), "x").unwrap();
        let store = SqliteServerConfigStore::open(&path).unwrap();
        assert!(store.load_raw().unwrap().is_some());
        assert_eq!(history.schema_version().unwrap(), 1, "history schema untouched");
    }

    #[test]
    fn unreadable_rows_are_errors() {
        let (_dir, store) = open();
        store
            .lock()
            .unwrap()
            .execute("INSERT INTO server_settings (config_json, created_at) VALUES ('{\"nope\":1}', 'now')", [])
            .unwrap();
        assert!(matches!(store.load_raw(), Err(SettingsError::Storage(m)) if m.contains("version 1")));
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        drop(SqliteServerConfigStore::open(&path).unwrap());
        Connection::open(&path).unwrap().execute("INSERT INTO settings_schema_version VALUES (99)", []).unwrap();
        assert!(SqliteServerConfigStore::open(&path).is_err());
    }
}
