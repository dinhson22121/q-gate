use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::redteam::application::{RedteamError, ScanStore};
use crate::redteam::domain::ScanReport;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const MIGRATIONS: &[&str] = &["
    CREATE TABLE scans (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        target_host   TEXT    NOT NULL,
        started_at_ms INTEGER NOT NULL,
        status        TEXT    NOT NULL,
        findings      INTEGER NOT NULL,
        report_json   TEXT    NOT NULL
    );
    CREATE INDEX idx_scans_started_at ON scans (started_at_ms DESC);
"];

pub struct SqliteScanStore {
    conn: Mutex<Connection>,
}

fn storage(err: impl std::fmt::Display) -> RedteamError {
    RedteamError::Storage(err.to_string())
}

impl SqliteScanStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RedteamError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(storage)?;
        }
        let mut conn = Connection::open(path).map_err(storage)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)).map_err(storage)?;
        migrate(&mut conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, RedteamError> {
        self.conn.lock().map_err(|_| RedteamError::Storage("database lock poisoned".into()))
    }
}

fn current_version(conn: &Connection) -> Result<u32, RedteamError> {
    conn.query_row("SELECT COALESCE(MAX(version), 0) FROM redteam_schema_version", [], |r| r.get(0)).map_err(storage)
}

fn migrate(conn: &mut Connection) -> Result<(), RedteamError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS redteam_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied = current_version(&tx)? as usize;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO redteam_schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn read_report(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanReport> {
    let id: i64 = row.get(0)?;
    let report_json: String = row.get(1)?;
    let mut report: ScanReport = serde_json::from_str(&report_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e)))?;
    report.id = Some(id);
    Ok(report)
}

impl ScanStore for SqliteScanStore {
    fn save(&self, report: &ScanReport) -> Result<i64, RedteamError> {
        let report_json = serde_json::to_string(report).map_err(storage)?;
        let conn = self.lock()?;
        conn.prepare_cached(
            "INSERT INTO scans (target_host, started_at_ms, status, findings, report_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .and_then(|mut stmt| {
            stmt.execute(params![
                report.target.base_url,
                report.started_at.timestamp_millis(),
                report.status.as_str(),
                report.findings.len() as i64,
                report_json,
            ])
        })
        .map_err(storage)?;
        Ok(conn.last_insert_rowid())
    }

    fn get(&self, id: i64) -> Result<Option<ScanReport>, RedteamError> {
        let conn = self.lock()?;
        conn.query_row("SELECT id, report_json FROM scans WHERE id = ?1", [id], read_report).optional().map_err(storage)
    }

    fn list(&self, limit: u32, offset: u32) -> Result<Vec<ScanReport>, RedteamError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare_cached("SELECT id, report_json FROM scans ORDER BY started_at_ms DESC, id DESC LIMIT ?1 OFFSET ?2")
            .map_err(storage)?;
        let rows = stmt.query_map(params![limit, offset], read_report).map_err(storage)?;
        rows.map(|r| r.map_err(storage)).collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::redteam::domain::{ScanStatus, ScanTarget};

    fn report(host: &str) -> ScanReport {
        let target = ScanTarget { base_url: host.to_string(), ..ScanTarget::default() };
        ScanReport {
            id: None,
            target: target.redacted(),
            findings: vec![],
            notes: vec!["n".into()],
            started_at: Utc::now(),
            finished_at: Utc::now(),
            cost_usd: 0.25,
            http_requests: 3,
            model_calls: 1,
            status: ScanStatus::Completed,
        }
    }

    fn open_temp() -> (tempfile::TempDir, SqliteScanStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteScanStore::open(dir.path().join("nested/qgate.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn round_trips_a_report() {
        let (_dir, store) = open_temp();
        let id = store.save(&report("http://10.0.0.1")).unwrap();
        let stored = store.get(id).unwrap().expect("stored report");
        assert_eq!(stored.id, Some(id));
        assert_eq!(stored.target.base_url, "http://10.0.0.1");
        assert_eq!(stored.notes, vec!["n".to_string()]);
        assert_eq!(stored.cost_usd, 0.25);
        assert!(store.get(id + 1).unwrap().is_none());
    }

    #[test]
    fn lists_newest_first_and_paginates() {
        let (_dir, store) = open_temp();
        let mut ids = Vec::new();
        for i in 0..3 {
            std::thread::sleep(Duration::from_millis(2));
            let mut r = report(&format!("http://10.0.0.{i}"));
            r.started_at = Utc::now();
            ids.push(store.save(&r).unwrap());
        }
        let page = store.list(2, 0).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, Some(ids[2]));
        let next = store.list(2, 2).unwrap();
        assert_eq!(next.len(), 1);
    }

    #[test]
    fn migration_is_idempotent_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        SqliteScanStore::open(&path).unwrap().save(&report("http://a")).unwrap();
        let reopened = SqliteScanStore::open(&path).unwrap();
        assert_eq!(reopened.list(10, 0).unwrap().len(), 1);
    }

    #[test]
    fn migrations_run_when_another_context_owns_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let seed = Connection::open(&path).unwrap();
        seed.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY);\
             INSERT INTO schema_version VALUES (1), (2), (3);",
        )
        .unwrap();
        let store = SqliteScanStore::open(&path).unwrap();
        let id = store.save(&report("http://a")).unwrap();
        assert_eq!(store.list(10, 0).unwrap().len(), 1);
        assert!(store.get(id).unwrap().is_some());
    }
}
