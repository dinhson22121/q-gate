use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::feedback::application::{CriterionStore, FeedbackError, FeedbackStore};
use crate::feedback::domain::criterion::{CriterionVerdict, CriterionVerification};
use crate::feedback::domain::model::{Judgment, Scope, Verdict};
use crate::feedback::domain::scoring::KindEvent;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const NEXT_SEQ: &str = "SELECT COALESCE(MAX(seq), 0) + 1 FROM
    (SELECT seq FROM feedback_judgments UNION ALL SELECT seq FROM feedback_resets)";

const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE feedback_judgments (
        repo        TEXT    NOT NULL,
        fingerprint TEXT    NOT NULL,
        kind        TEXT    NOT NULL,
        verdict     TEXT    NOT NULL,
        seq         INTEGER NOT NULL,
        recorded_at TEXT    NOT NULL,
        PRIMARY KEY (repo, fingerprint)
    );
    CREATE INDEX feedback_judgments_seq ON feedback_judgments (repo, seq);
    CREATE TABLE feedback_resets (
        repo TEXT    NOT NULL,
        kind TEXT    NOT NULL,
        seq  INTEGER NOT NULL,
        PRIMARY KEY (repo, kind)
    );
",
    "
    ALTER TABLE feedback_judgments ADD COLUMN scope TEXT NOT NULL DEFAULT 'repo';
    CREATE INDEX feedback_judgments_global ON feedback_judgments (fingerprint) WHERE scope = 'global';
",
    "
    CREATE TABLE criterion_verifications (
        repo           TEXT NOT NULL,
        criterion_id   TEXT NOT NULL,
        text_hash      TEXT NOT NULL,
        criterion_text TEXT NOT NULL,
        verdict        TEXT NOT NULL,
        note           TEXT NOT NULL,
        recorded_at    TEXT NOT NULL,
        PRIMARY KEY (repo, criterion_id, text_hash)
    );
",
];

const FIND_EFFECTIVE: &str = "SELECT verdict, scope, recorded_at, repo FROM feedback_judgments
    WHERE fingerprint = ?2 AND (repo = ?1 OR scope = 'global')
    ORDER BY repo = ?1 DESC, seq DESC LIMIT 1";

fn storage(err: impl std::fmt::Display) -> FeedbackError {
    FeedbackError::Storage(err.to_string())
}

pub struct SqliteFeedbackStore {
    conn: Mutex<Connection>,
}

impl SqliteFeedbackStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FeedbackError> {
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

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, FeedbackError> {
        self.conn.lock().map_err(|_| FeedbackError::Storage("database lock poisoned".into()))
    }
}

fn current_version(conn: &Connection) -> Result<usize, FeedbackError> {
    conn.query_row("SELECT COALESCE(MAX(version), 0) FROM feedback_schema_version", [], |r| r.get::<_, i64>(0))
        .map(|v| v as usize)
        .map_err(storage)
}

fn migrate(conn: &mut Connection) -> Result<(), FeedbackError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS feedback_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied = current_version(&tx)?;
    if applied > MIGRATIONS.len() {
        return Err(FeedbackError::Storage(format!(
            "feedback schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO feedback_schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn parse_verdict(value: &str) -> Result<Verdict, FeedbackError> {
    Verdict::parse(value).ok_or_else(|| FeedbackError::Storage(format!("unknown verdict '{value}'")))
}

fn parse_scope(value: &str) -> Result<Scope, FeedbackError> {
    Scope::parse(value).ok_or_else(|| FeedbackError::Storage(format!("unknown scope '{value}'")))
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, FeedbackError> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| FeedbackError::Storage(format!("invalid recorded_at '{value}': {e}")))
}

impl FeedbackStore for SqliteFeedbackStore {
    fn put(
        &self,
        repo: &str,
        fingerprint: &str,
        kind: &str,
        verdict: Verdict,
        scope: Scope,
        recorded_at: DateTime<Utc>,
    ) -> Result<(), FeedbackError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(storage)?;
        let seq: i64 = tx.query_row(NEXT_SEQ, [], |r| r.get(0)).map_err(storage)?;
        tx.execute(
            "INSERT INTO feedback_judgments (repo, fingerprint, kind, verdict, seq, recorded_at, scope)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(repo, fingerprint) DO UPDATE SET kind = excluded.kind, verdict = excluded.verdict,
                                                         seq = excluded.seq, recorded_at = excluded.recorded_at,
                                                         scope = excluded.scope",
            params![repo, fingerprint, kind, verdict.as_str(), seq, recorded_at.to_rfc3339(), scope.as_str()],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }

    fn delete(&self, repo: &str, fingerprint: &str) -> Result<bool, FeedbackError> {
        let conn = self.lock()?;
        let removed = conn
            .execute(
                "DELETE FROM feedback_judgments WHERE fingerprint = ?2 AND (repo = ?1 OR scope = 'global')",
                params![repo, fingerprint],
            )
            .map_err(storage)?;
        Ok(removed > 0)
    }

    fn find(&self, repo: &str, fingerprints: &[String]) -> Result<Vec<Judgment>, FeedbackError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(FIND_EFFECTIVE).map_err(storage)?;
        let mut found = Vec::new();
        for fingerprint in fingerprints {
            let row: Option<(String, String, String, String)> = stmt
                .query_row(params![repo, fingerprint], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .optional()
                .map_err(storage)?;
            if let Some((verdict, scope, recorded_at, owner)) = row {
                found.push(Judgment {
                    repo: owner,
                    fingerprint: fingerprint.clone(),
                    verdict: parse_verdict(&verdict)?,
                    scope: parse_scope(&scope)?,
                    recorded_at: parse_time(&recorded_at)?,
                });
            }
        }
        Ok(found)
    }

    fn events(&self, repo: &str) -> Result<Vec<KindEvent>, FeedbackError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT j.kind, j.verdict FROM feedback_judgments j
                 WHERE j.repo = ?1
                   AND j.seq > COALESCE((SELECT r.seq FROM feedback_resets r WHERE r.repo = j.repo AND r.kind = j.kind), 0)
                 ORDER BY j.seq",
            )
            .map_err(storage)?;
        let rows = stmt
            .query_map([repo], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        rows.into_iter().map(|(kind, verdict)| Ok(KindEvent { kind, verdict: parse_verdict(&verdict)? })).collect()
    }

    fn reset(&self, repo: &str, kind: &str) -> Result<(), FeedbackError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(storage)?;
        let seq: i64 = tx.query_row(NEXT_SEQ, [], |r| r.get(0)).map_err(storage)?;
        tx.execute(
            "INSERT INTO feedback_resets (repo, kind, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(repo, kind) DO UPDATE SET seq = excluded.seq",
            params![repo, kind, seq],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }
}

const CRITERION_COLUMNS: &str = "repo, criterion_id, text_hash, criterion_text, verdict, note, recorded_at";

type CriterionRow = (String, String, String, String, String, String, String);

fn criterion_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CriterionRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
}

fn to_verification(row: CriterionRow) -> Result<CriterionVerification, FeedbackError> {
    let (repo, criterion_id, text_hash, criterion_text, verdict, note, recorded_at) = row;
    let verdict = CriterionVerdict::parse(&verdict)
        .ok_or_else(|| FeedbackError::Storage(format!("unknown criterion verdict '{verdict}'")))?;
    Ok(CriterionVerification {
        repo,
        criterion_id,
        text_hash,
        criterion_text,
        verdict,
        note,
        recorded_at: parse_time(&recorded_at)?,
    })
}

impl CriterionStore for SqliteFeedbackStore {
    fn put_criterion(&self, v: &CriterionVerification) -> Result<(), FeedbackError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO criterion_verifications (repo, criterion_id, text_hash, criterion_text, verdict, note, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(repo, criterion_id, text_hash) DO UPDATE SET criterion_text = excluded.criterion_text,
                 verdict = excluded.verdict, note = excluded.note, recorded_at = excluded.recorded_at",
            params![
                v.repo,
                v.criterion_id,
                v.text_hash,
                v.criterion_text,
                v.verdict.as_str(),
                v.note,
                v.recorded_at.to_rfc3339()
            ],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn delete_criterion(&self, repo: &str, criterion_id: &str, text_hash: &str) -> Result<bool, FeedbackError> {
        let conn = self.lock()?;
        let removed = conn
            .execute(
                "DELETE FROM criterion_verifications WHERE repo = ?1 AND criterion_id = ?2 AND text_hash = ?3",
                params![repo, criterion_id, text_hash],
            )
            .map_err(storage)?;
        Ok(removed > 0)
    }

    fn list_criteria(&self, repo: &str) -> Result<Vec<CriterionVerification>, FeedbackError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {CRITERION_COLUMNS} FROM criterion_verifications WHERE repo = ?1
                 ORDER BY recorded_at DESC, criterion_id"
            ))
            .map_err(storage)?;
        let rows =
            stmt.query_map([repo], criterion_row).map_err(storage)?.collect::<Result<Vec<_>, _>>().map_err(storage)?;
        rows.into_iter().map(to_verification).collect()
    }

    fn find_criteria(
        &self,
        repo: &str,
        keys: &[(String, String)],
    ) -> Result<Vec<CriterionVerification>, FeedbackError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {CRITERION_COLUMNS} FROM criterion_verifications
                 WHERE repo = ?1 AND criterion_id = ?2 AND text_hash = ?3"
            ))
            .map_err(storage)?;
        let mut found = Vec::new();
        for (criterion_id, text_hash) in keys {
            let row =
                stmt.query_row(params![repo, criterion_id, text_hash], criterion_row).optional().map_err(storage)?;
            if let Some(row) = row {
                found.push(to_verification(row)?);
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;
