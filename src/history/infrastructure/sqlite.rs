use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::history::application::{HistoryError, RunFilter, RunRepository};
use crate::history::domain::{ReportView, ReviewRun, RunProvenance, RunStatus, StoredRun, TicketKey, Trigger};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const UNREADABLE_REPORT: &str = "stored report could not be read";

const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE runs (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        repo_path     TEXT    NOT NULL,
        repo_name     TEXT    NOT NULL,
        commit_sha    TEXT,
        branch        TEXT,
        ticket_key    TEXT,
        trigger_kind  TEXT    NOT NULL,
        created_at_ms INTEGER NOT NULL,
        status        TEXT    NOT NULL,
        error         TEXT,
        report_json   TEXT
    );
    CREATE INDEX idx_runs_created_at ON runs (created_at_ms DESC);
    CREATE INDEX idx_runs_repo ON runs (repo_name, created_at_ms DESC);
    CREATE INDEX idx_runs_status ON runs (status, created_at_ms DESC);
",
    "ALTER TABLE runs ADD COLUMN report_md TEXT;",
    "ALTER TABLE runs ADD COLUMN provenance_json TEXT;",
];

const COLUMNS: &str = "id, repo_path, repo_name, commit_sha, branch, ticket_key, trigger_kind, \
                       created_at_ms, status, error, report_json, report_md, provenance_json";
const FILTER: &str = "(?1 IS NULL OR repo_name = ?1 OR repo_path = ?1) AND (?2 IS NULL OR status = ?2)";

pub struct SqliteRunRepository {
    conn: Mutex<Connection>,
}

fn storage(err: impl std::fmt::Display) -> HistoryError {
    HistoryError::Storage(err.to_string())
}

impl SqliteRunRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, HistoryError> {
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

    pub fn schema_version(&self) -> Result<u32, HistoryError> {
        let conn = self.lock()?;
        current_version(&conn)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, HistoryError> {
        self.conn.lock().map_err(|_| HistoryError::Storage("database lock poisoned".into()))
    }

    fn query_runs(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<StoredRun>, HistoryError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(sql).map_err(storage)?;
        let rows = stmt.query_map(params, read_row).map_err(storage)?;
        rows.map(|r| r.map_err(storage)).collect()
    }
}

fn current_version(conn: &Connection) -> Result<u32, HistoryError> {
    conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0)).map_err(storage)
}

fn migrate(conn: &mut Connection) -> Result<(), HistoryError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied = current_version(&tx)? as usize;
    if applied > MIGRATIONS.len() {
        return Err(HistoryError::Storage(format!(
            "database schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn invalid_column(index: usize, err: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
}

fn read_row(row: &Row<'_>) -> rusqlite::Result<StoredRun> {
    let trigger: String = row.get(6)?;
    let created_at_ms: i64 = row.get(7)?;
    let status: String = row.get(8)?;
    let mut error: Option<String> = row.get(9)?;
    let report_json: Option<String> = row.get(10)?;
    let report = report_json.and_then(|json| match serde_json::from_str::<ReportView>(&json) {
        Ok(report) => Some(report),
        Err(_) => {
            error.get_or_insert_with(|| UNREADABLE_REPORT.into());
            None
        }
    });
    let provenance_json: Option<String> = row.get(12)?;
    let provenance = provenance_json.and_then(|json| serde_json::from_str::<RunProvenance>(&json).ok());
    Ok(ReviewRun {
        id: row.get(0)?,
        repo_path: row.get(1)?,
        repo_name: row.get(2)?,
        commit: row.get(3)?,
        branch: row.get(4)?,
        ticket_key: row.get(5)?,
        trigger: trigger.parse::<Trigger>().map_err(|e| invalid_column(6, e))?,
        created_at: DateTime::<Utc>::from_timestamp_millis(created_at_ms)
            .ok_or_else(|| rusqlite::Error::IntegralValueOutOfRange(7, created_at_ms))?,
        status: status.parse::<RunStatus>().map_err(|e| invalid_column(8, e))?,
        error,
        report,
        report_md: row.get(11)?,
        provenance,
    })
}

impl RunRepository for SqliteRunRepository {
    fn insert(&self, run: &ReviewRun) -> Result<i64, HistoryError> {
        let report_json = run.report.as_ref().map(serde_json::to_string).transpose().map_err(storage)?;
        let provenance_json = run.provenance.as_ref().map(serde_json::to_string).transpose().map_err(storage)?;
        let conn = self.lock()?;
        conn.prepare_cached(
            "INSERT INTO runs (repo_path, repo_name, commit_sha, branch, ticket_key, trigger_kind, \
             created_at_ms, status, error, report_json, report_md, provenance_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .and_then(|mut stmt| {
            stmt.execute(params![
                run.repo_path,
                run.repo_name,
                run.commit,
                run.branch,
                run.ticket_key,
                run.trigger.as_str(),
                run.created_at.timestamp_millis(),
                run.status.as_str(),
                run.error,
                report_json,
                run.report_md,
                provenance_json,
            ])
        })
        .map_err(storage)?;
        Ok(conn.last_insert_rowid())
    }

    fn get(&self, id: i64) -> Result<Option<StoredRun>, HistoryError> {
        let conn = self.lock()?;
        conn.query_row(&format!("SELECT {COLUMNS} FROM runs WHERE id = ?1"), [id], read_row).optional().map_err(storage)
    }

    fn list(&self, filter: &RunFilter) -> Result<Vec<StoredRun>, HistoryError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM runs WHERE {FILTER} ORDER BY created_at_ms DESC, id DESC LIMIT ?3 OFFSET ?4"
        );
        self.query_runs(&sql, params![filter.repo, filter.status.map(RunStatus::as_str), filter.limit, filter.offset])
    }

    fn count(&self, filter: &RunFilter) -> Result<u64, HistoryError> {
        let conn = self.lock()?;
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM runs WHERE {FILTER}"),
                params![filter.repo, filter.status.map(RunStatus::as_str)],
                |r| r.get(0),
            )
            .map_err(storage)?;
        u64::try_from(count).map_err(storage)
    }

    fn since(&self, since: DateTime<Utc>) -> Result<Vec<StoredRun>, HistoryError> {
        let sql = format!("SELECT {COLUMNS} FROM runs WHERE created_at_ms >= ?1 ORDER BY created_at_ms DESC, id DESC");
        self.query_runs(&sql, [since.timestamp_millis()])
    }

    fn for_ticket(&self, key: &TicketKey, limit: u32) -> Result<Vec<StoredRun>, HistoryError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM runs WHERE ticket_key = ?1 OR instr(report_json, ?2) > 0 OR instr(report_json, ?3) > 0 \
             ORDER BY created_at_ms DESC, id DESC LIMIT ?4"
        );
        let key = key.as_str();
        self.query_runs(&sql, params![key, format!("\"{key}\""), format!("\"{key}/"), limit])
    }

    fn repos(&self) -> Result<Vec<String>, HistoryError> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare_cached("SELECT DISTINCT repo_name FROM runs ORDER BY repo_name").map_err(storage)?;
        let rows = stmt.query_map([], |r| r.get(0)).map_err(storage)?;
        rows.map(|r| r.map_err(storage)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::domain::fixtures::{at, live_run};
    use crate::review::application::{CriterionStatus, Severity};

    fn open_temp() -> (tempfile::TempDir, SqliteRunRepository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteRunRepository::open(dir.path().join("nested/dir/qgate.db")).unwrap();
        (dir, repo)
    }

    fn filter(repo: Option<&str>, status: Option<RunStatus>, limit: u32, offset: u32) -> RunFilter {
        RunFilter { repo: repo.map(str::to_string), status, limit, offset }
    }

    #[test]
    fn round_trips_a_full_run() {
        let (_dir, repo) = open_temp();
        let run = live_run("api", at(10, 9));

        let id = repo.insert(&run).unwrap();
        let stored = repo.get(id).unwrap().expect("stored run");

        assert_eq!(stored.id, id);
        assert_eq!(stored.repo_name, "api");
        assert_eq!(stored.commit, run.commit);
        assert_eq!(stored.branch, run.branch);
        assert_eq!(stored.ticket_key.as_deref(), Some("QG-7"));
        assert_eq!(stored.trigger, Trigger::Manual);
        assert_eq!(stored.created_at, run.created_at);
        assert_eq!(stored.status, RunStatus::Failed);
        let report = stored.report.expect("report");
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.findings[0].evidence, "query(&format!(\"{}\", input))");
        let rule = report.findings[0].prevention_rule.as_ref().expect("prevention rule round-trips");
        assert_eq!((rule.text.as_str(), rule.scope.as_deref()), ("Bind <b>parameters</b>", Some("**/*.rs")));
        assert_eq!(report.rejected[0].reason, "evidence_not_found");
        assert_eq!(report.skipped[0].reason.kind, "binary");
        let req = report.requirements.expect("requirements");
        assert_eq!(req.requirements.criteria[0].text, "Users can log in");
        assert_eq!(req.assessments[0].status, CriterionStatus::NotMet);
        assert_eq!(req.assessments[0].evidence[0].line, 2);
        assert_eq!(report.checks[0].output_tail, "error: unused");
        assert_eq!(report.gate.failed_checks, vec!["clippy".to_string()]);
        assert_eq!(report.usage.input_tokens, 100);
        assert_eq!(report.estimated_cost_usd, Some(0.042));
        assert!(repo.get(id + 1).unwrap().is_none());
    }

    #[test]
    fn provenance_round_trips_and_old_rows_read_as_none() {
        let (_dir, repo) = open_temp();
        let provenance = RunProvenance {
            model: "claude-opus-5-5".into(),
            claude_cli_version: "2.1.0".into(),
            prompt_hash: "ab".repeat(32),
        };
        let with =
            repo.insert(&ReviewRun { provenance: Some(provenance.clone()), ..live_run("api", at(1, 0)) }).unwrap();
        let without = repo.insert(&live_run("api", at(2, 0))).unwrap();
        assert_eq!(repo.get(with).unwrap().unwrap().provenance, Some(provenance));
        assert_eq!(repo.get(without).unwrap().unwrap().provenance, None);
        repo.lock().unwrap().execute("UPDATE runs SET provenance_json = '{bad' WHERE id = ?1", [with]).unwrap();
        assert_eq!(repo.get(with).unwrap().unwrap().provenance, None);
    }

    #[test]
    fn stores_errored_runs_without_report() {
        let (_dir, repo) = open_temp();
        let run = ReviewRun {
            status: RunStatus::Error,
            error: Some("api down".into()),
            report: None,
            commit: None,
            branch: None,
            ticket_key: None,
            ..live_run("api", at(1, 0))
        };
        let stored = repo.get(repo.insert(&run).unwrap()).unwrap().unwrap();
        assert_eq!(stored.status, RunStatus::Error);
        assert_eq!(stored.error.as_deref(), Some("api down"));
        assert!(stored.report.is_none() && stored.commit.is_none());
    }

    #[test]
    fn filters_and_paginates_newest_first() {
        let (_dir, repo) = open_temp();
        for (i, name) in ["api", "web", "api", "api", "web"].iter().enumerate() {
            let mut run = live_run(name, at(1 + i as u32, 0));
            if i == 3 {
                run.status = RunStatus::Passed;
            }
            repo.insert(&run).unwrap();
        }

        let all = repo.list(&filter(None, None, 10, 0)).unwrap();
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5, 4, 3, 2, 1]);

        let api = repo.list(&filter(Some("api"), None, 10, 0)).unwrap();
        assert_eq!(api.iter().map(|r| r.id).collect::<Vec<_>>(), vec![4, 3, 1]);
        assert_eq!(repo.count(&filter(Some("/src/api"), None, 1, 0)).unwrap(), 3);

        let passed = repo.list(&filter(Some("api"), Some(RunStatus::Passed), 10, 0)).unwrap();
        assert_eq!(passed.iter().map(|r| r.id).collect::<Vec<_>>(), vec![4]);

        let page = repo.list(&filter(None, None, 2, 2)).unwrap();
        assert_eq!(page.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 2]);
        assert_eq!(repo.count(&filter(None, None, 2, 2)).unwrap(), 5);

        let recent = repo.since(at(4, 0)).unwrap();
        assert_eq!(recent.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5, 4]);
        assert_eq!(repo.repos().unwrap(), vec!["api".to_string(), "web".to_string()]);
    }

    #[test]
    fn filter_values_are_bound_not_interpolated() {
        let (_dir, repo) = open_temp();
        repo.insert(&live_run("api", at(1, 0))).unwrap();
        let hostile = filter(Some("' OR 1=1 --"), None, 10, 0);
        assert!(repo.list(&hostile).unwrap().is_empty());
        assert_eq!(repo.count(&hostile).unwrap(), 0);
    }

    #[test]
    fn migration_is_idempotent_and_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let first = SqliteRunRepository::open(&path).unwrap();
        first.insert(&live_run("api", at(1, 0))).unwrap();
        assert_eq!(first.schema_version().unwrap(), MIGRATIONS.len() as u32);
        drop(first);

        let second = SqliteRunRepository::open(&path).unwrap();
        assert_eq!(second.schema_version().unwrap(), MIGRATIONS.len() as u32);
        assert_eq!(second.count(&filter(None, None, 1, 0)).unwrap(), 1);
        let rows: i64 =
            second.lock().unwrap().query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, MIGRATIONS.len() as i64);
        let mode: String = second.lock().unwrap().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn refuses_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        drop(SqliteRunRepository::open(&path).unwrap());
        Connection::open(&path).unwrap().execute("INSERT INTO schema_version VALUES (99)", []).unwrap();
        assert!(matches!(SqliteRunRepository::open(&path), Err(HistoryError::Storage(_))));
    }

    #[test]
    fn unreadable_report_keeps_the_run_visible() {
        let (_dir, repo) = open_temp();
        let id = repo.insert(&live_run("api", at(1, 0))).unwrap();
        repo.lock().unwrap().execute("UPDATE runs SET report_json = '{broken' WHERE id = ?1", [id]).unwrap();
        let stored = repo.get(id).unwrap().unwrap();
        assert!(stored.report.is_none());
        assert_eq!(stored.error.as_deref(), Some(UNREADABLE_REPORT));
    }

    #[test]
    fn for_ticket_prefilters_by_column_and_quoted_json_keys() {
        let (_dir, repo) = open_temp();
        let set_json = |id: i64, json: &str| {
            repo.lock().unwrap().execute("UPDATE runs SET report_json = ?1 WHERE id = ?2", params![json, id]).unwrap();
        };
        let mut ids = Vec::new();
        for day in 1..=5 {
            ids.push(repo.insert(&ReviewRun { ticket_key: None, ..live_run("api", at(day, 0)) }).unwrap());
        }
        let by_column = repo.insert(&live_run("api", at(6, 0))).unwrap();
        set_json(ids[0], r#"{"gate":{"passed":true},"ticket":{"keys":["SHOP-7"]}}"#);
        set_json(
            ids[1],
            r#"{"gate":{"passed":true},"requirements":{"requirements":{"key":"SHOP-8"},"assessments":[{"criterion_id":"SHOP-7/AC-1","status":"met"}]}}"#,
        );
        set_json(ids[2], r#"{"gate":{"passed":true},"ticket":{"keys":["SHOP-70"]}}"#);
        set_json(ids[3], r#"{"gate":{"passed":true},"ticket":{"description":"see \"SHOP-7\" and SHOP-7"}}"#);

        let shop7 = TicketKey::parse("SHOP-7").unwrap();
        let found: Vec<i64> = repo.for_ticket(&shop7, 10).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(found, vec![ids[1], ids[0]]);
        assert_eq!(repo.for_ticket(&shop7, 1).unwrap().len(), 1);

        let qg7 = TicketKey::parse("QG-7").unwrap();
        let found: Vec<i64> = repo.for_ticket(&qg7, 10).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(found, vec![by_column, ids[4]], "column match and the stored live report's requirements key");
    }
}
