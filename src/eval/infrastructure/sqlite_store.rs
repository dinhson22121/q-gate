use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::eval::application::{EvalError, EvalStore};
use crate::eval::domain::run::{AggregateMetrics, EvalRun};
use crate::eval::domain::scoring::CaseScore;
use crate::history::application::RunProvenance;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE eval_runs (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at    TEXT    NOT NULL,
        provenance    TEXT    NOT NULL,
        runs_per_case INTEGER NOT NULL,
        case_count    INTEGER NOT NULL,
        with_tests    INTEGER NOT NULL,
        metrics       TEXT    NOT NULL,
        cost_usd      REAL    NOT NULL
    );
    CREATE TABLE eval_case_results (
        run_id     INTEGER NOT NULL REFERENCES eval_runs(id) ON DELETE CASCADE,
        case_id    TEXT    NOT NULL,
        repetition INTEGER NOT NULL,
        result     TEXT    NOT NULL,
        PRIMARY KEY (run_id, case_id, repetition)
    );
",
    "ALTER TABLE eval_runs ADD COLUMN partial INTEGER NOT NULL DEFAULT 0;",
];

const RUN_COLUMNS: &str = "id, created_at, provenance, runs_per_case, with_tests, metrics, partial";

fn storage(err: impl std::fmt::Display) -> EvalError {
    EvalError::Storage(err.to_string())
}

pub struct SqliteEvalStore {
    conn: Mutex<Connection>,
}

impl SqliteEvalStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EvalError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(storage)?;
        }
        let mut conn = Connection::open(path).map_err(storage)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)).map_err(storage)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(storage)?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(storage)?;
        migrate(&mut conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn runs_where(&self, clause: &str, args: impl rusqlite::Params) -> Result<Vec<EvalRun>, EvalError> {
        let conn = self.conn();
        let sql = format!("SELECT {RUN_COLUMNS} FROM eval_runs {clause}");
        let mut statement = conn.prepare(&sql).map_err(storage)?;
        let headers =
            statement.query_map(args, read_header).map_err(storage)?.collect::<Result<Vec<_>, _>>().map_err(storage)?;
        headers.into_iter().map(|header| attach_results(&conn, header)).collect()
    }
}

struct Header {
    id: i64,
    created_at: String,
    provenance: String,
    runs_per_case: u32,
    with_tests: bool,
    metrics: String,
    partial: bool,
}

fn read_header(row: &Row<'_>) -> rusqlite::Result<Header> {
    Ok(Header {
        id: row.get(0)?,
        created_at: row.get(1)?,
        provenance: row.get(2)?,
        runs_per_case: row.get(3)?,
        with_tests: row.get::<_, i64>(4)? != 0,
        metrics: row.get(5)?,
        partial: row.get::<_, i64>(6)? != 0,
    })
}

fn attach_results(conn: &Connection, header: Header) -> Result<EvalRun, EvalError> {
    let mut statement =
        conn.prepare("SELECT result FROM eval_case_results WHERE run_id = ?1 ORDER BY rowid").map_err(storage)?;
    let rows = statement.query_map([header.id], |row| row.get::<_, String>(0)).map_err(storage)?;
    let results = rows
        .map(|row| {
            let text = row.map_err(storage)?;
            serde_json::from_str::<CaseScore>(&text).map_err(storage)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EvalRun {
        id: header.id,
        created_at: DateTime::parse_from_rfc3339(&header.created_at).map(|t| t.with_timezone(&Utc)).map_err(storage)?,
        provenance: serde_json::from_str::<RunProvenance>(&header.provenance).map_err(storage)?,
        runs_per_case: header.runs_per_case,
        with_tests: header.with_tests,
        partial: header.partial,
        metrics: serde_json::from_str::<AggregateMetrics>(&header.metrics).map_err(storage)?,
        results,
    })
}

impl EvalStore for SqliteEvalStore {
    fn save(&self, run: &EvalRun) -> Result<i64, EvalError> {
        let provenance = serde_json::to_string(&run.provenance).map_err(storage)?;
        let metrics = serde_json::to_string(&run.metrics).map_err(storage)?;
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(storage)?;
        tx.execute(
            "INSERT INTO eval_runs (created_at, provenance, runs_per_case, case_count, with_tests, metrics, cost_usd, partial)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.created_at.to_rfc3339(),
                provenance,
                run.runs_per_case,
                run.metrics.case_count,
                run.with_tests,
                metrics,
                run.metrics.cost_usd,
                run.partial
            ],
        )
        .map_err(storage)?;
        let id = tx.last_insert_rowid();
        for result in &run.results {
            let json = serde_json::to_string(result).map_err(storage)?;
            tx.execute(
                "INSERT INTO eval_case_results (run_id, case_id, repetition, result) VALUES (?1, ?2, ?3, ?4)",
                params![id, result.case_id, result.repetition, json],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(id)
    }

    fn get(&self, id: i64) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs_where("WHERE id = ?1", [id])?.into_iter().next())
    }

    fn latest(&self) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs_where("ORDER BY id DESC LIMIT 1", [])?.into_iter().next())
    }

    fn latest_for(&self, provenance: &RunProvenance) -> Result<Option<EvalRun>, EvalError> {
        let runs = self.runs_where(
            "WHERE json_extract(provenance, '$.model') = ?1
               AND json_extract(provenance, '$.claude_cli_version') = ?2
               AND json_extract(provenance, '$.prompt_hash') = ?3
             ORDER BY id DESC LIMIT 1",
            params![provenance.model, provenance.claude_cli_version, provenance.prompt_hash],
        )?;
        Ok(runs.into_iter().next())
    }

    fn latest_full(&self) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs_where("WHERE partial = 0 ORDER BY id DESC LIMIT 1", [])?.into_iter().next())
    }

    fn previous_full_to(&self, id: i64) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs_where("WHERE id < ?1 AND partial = 0 ORDER BY id DESC LIMIT 1", [id])?.into_iter().next())
    }

    fn list(&self, limit: u32) -> Result<Vec<EvalRun>, EvalError> {
        self.runs_where("ORDER BY id DESC LIMIT ?1", [limit])
    }
}

fn migrate(conn: &mut Connection) -> Result<(), EvalError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS eval_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied = tx
        .query_row("SELECT MAX(version) FROM eval_schema_version", [], |row| row.get::<_, Option<i64>>(0))
        .optional()
        .map_err(storage)?
        .flatten()
        .unwrap_or(0) as usize;
    if applied > MIGRATIONS.len() {
        return Err(EvalError::Storage(format!(
            "eval schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO eval_schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::application::test_support::{provenance, sample_run};
    use crate::eval::domain::fixtures::score;

    fn store() -> (tempfile::TempDir, SqliteEvalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEvalStore::open(dir.path().join("qgate.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_saved_run_round_trips_with_its_results_in_order() {
        let (_dir, store) = store();
        let mut run = sample_run(&[true, false]);
        run.results = vec![score("b", 0, &[true], 0), score("a", 0, &[false], 1), score("b", 1, &[true], 0)];
        run.metrics = crate::eval::domain::run::aggregate(&run.results, 2);
        run.runs_per_case = 2;
        run.with_tests = true;

        let id = store.save(&run).unwrap();
        let loaded = store.get(id).unwrap().unwrap();

        assert_eq!(loaded, EvalRun { id, ..run });
        assert_eq!(loaded.provenance, provenance());
    }

    #[test]
    fn latest_for_matches_the_whole_provenance_not_just_the_newest_run() {
        let (_dir, store) = store();
        let first = store.save(&sample_run(&[true])).unwrap();
        let other = RunProvenance { model: "opus".into(), ..provenance() };
        store.save(&EvalRun::new(chrono::Utc::now(), other.clone(), 1, false, Vec::new())).unwrap();

        let for_default = store.latest_for(&provenance()).unwrap().unwrap();
        let for_other = store.latest_for(&other).unwrap().unwrap();
        let stale = RunProvenance { prompt_hash: "x".into(), ..provenance() };

        assert_eq!(for_default.id, first);
        assert_eq!(for_other.provenance.model, "opus");
        assert!(store.latest_for(&stale).unwrap().is_none());
    }

    #[test]
    fn a_stored_result_without_a_content_hash_still_loads_as_unknown() {
        let mut value = serde_json::to_value(score("a", 0, &[true], 0)).unwrap();
        value.as_object_mut().unwrap().remove("content_hash");

        let loaded: CaseScore = serde_json::from_value(value).unwrap();

        assert!(loaded.content_hash.is_empty());
    }

    #[test]
    fn latest_previous_and_list_follow_insertion_order() {
        let (_dir, store) = store();
        let ids: Vec<i64> = (0..3).map(|_| store.save(&sample_run(&[true])).unwrap()).collect();

        assert_eq!(store.latest().unwrap().unwrap().id, ids[2]);
        assert_eq!(store.previous_full_to(ids[2]).unwrap().unwrap().id, ids[1]);
        assert!(store.previous_full_to(ids[0]).unwrap().is_none());
        let listed: Vec<i64> = store.list(2).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(listed, [ids[2], ids[1]]);
        assert!(store.get(999).unwrap().is_none());
    }

    #[test]
    fn full_lookups_skip_partial_runs_and_the_flag_round_trips() {
        let (_dir, store) = store();
        let first = store.save(&sample_run(&[true])).unwrap();
        let partial = store.save(&sample_run(&[true]).with_partial(true)).unwrap();
        let last = store.save(&sample_run(&[true])).unwrap();

        assert_eq!(store.latest_full().unwrap().unwrap().id, last);
        assert_eq!(store.previous_full_to(last).unwrap().unwrap().id, first);
        assert_eq!(store.previous_full_to(partial).unwrap().unwrap().id, first);
        assert!(store.get(partial).unwrap().unwrap().partial);
        assert!(!store.get(first).unwrap().unwrap().partial);
    }

    #[test]
    fn only_partial_runs_leave_no_full_run() {
        let (_dir, store) = store();
        store.save(&sample_run(&[true]).with_partial(true)).unwrap();

        assert!(store.latest_full().unwrap().is_none());
    }

    #[test]
    fn a_database_from_before_the_partial_column_upgrades_with_runs_as_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch("CREATE TABLE eval_schema_version (version INTEGER NOT NULL PRIMARY KEY); INSERT INTO eval_schema_version VALUES (1);").unwrap();
        let run = sample_run(&[true]);
        conn.execute(
            "INSERT INTO eval_runs (created_at, provenance, runs_per_case, case_count, with_tests, metrics, cost_usd)
             VALUES (?1, ?2, 1, 1, 0, ?3, 0.0)",
            params![
                run.created_at.to_rfc3339(),
                serde_json::to_string(&run.provenance).unwrap(),
                serde_json::to_string(&run.metrics).unwrap()
            ],
        )
        .unwrap();
        drop(conn);

        let store = SqliteEvalStore::open(&path).unwrap();

        assert!(!store.get(1).unwrap().unwrap().partial);
        assert_eq!(store.latest_full().unwrap().unwrap().id, 1);
    }

    #[test]
    fn an_empty_store_has_no_runs() {
        let (_dir, store) = store();

        assert!(store.latest().unwrap().is_none());
        assert!(store.list(10).unwrap().is_empty());
    }

    #[test]
    fn reopening_keeps_data_and_does_not_reapply_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let id = SqliteEvalStore::open(&path).unwrap().save(&sample_run(&[true])).unwrap();

        let reopened = SqliteEvalStore::open(&path).unwrap();

        assert_eq!(reopened.get(id).unwrap().unwrap().id, id);
    }

    #[test]
    fn a_newer_schema_than_supported_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        SqliteEvalStore::open(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute("INSERT INTO eval_schema_version (version) VALUES (99)", []).unwrap();
        drop(conn);

        let error = SqliteEvalStore::open(&path).err().unwrap();

        assert!(matches!(error, EvalError::Storage(m) if m.contains("newer")));
    }

    #[test]
    fn corrupt_json_is_a_storage_error() {
        let (dir, store) = store();
        let id = store.save(&sample_run(&[true])).unwrap();
        Connection::open(dir.path().join("qgate.db"))
            .unwrap()
            .execute("UPDATE eval_runs SET metrics = 'nope' WHERE id = ?1", [id])
            .unwrap();

        assert!(matches!(store.get(id), Err(EvalError::Storage(_))));
    }
}
