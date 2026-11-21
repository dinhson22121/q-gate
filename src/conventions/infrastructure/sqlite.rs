//! SQLite `RulebookStore`: the rulebook is stored as JSON, decisions in their own table so they
//! survive rebuilds. Shares `qgate.db` with other contexts but only touches its own tables.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::conventions::application::{ConventionsError, RuleDecision, RulebookStore};
use crate::conventions::domain::rulebook::{RuleStatus, Rulebook};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Ordered migrations; index + 1 is the schema version.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE rulebooks (
        repo          TEXT    NOT NULL PRIMARY KEY,
        fingerprint   TEXT    NOT NULL,
        created_at    TEXT    NOT NULL,
        rulebook_json TEXT    NOT NULL
    );
    CREATE TABLE rule_decisions (
        repo             TEXT NOT NULL,
        normalized_title TEXT NOT NULL,
        status           TEXT NOT NULL,
        decided_at       TEXT NOT NULL,
        PRIMARY KEY (repo, normalized_title)
    );
"];

fn storage(err: impl std::fmt::Display) -> ConventionsError {
    ConventionsError::Storage(err.to_string())
}

pub struct SqliteRulebookStore {
    conn: Mutex<Connection>,
}

impl SqliteRulebookStore {
    /// Opens (creating parent directories and the file if needed) and migrates `<data_dir>/qgate.db`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ConventionsError> {
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

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, ConventionsError> {
        self.conn.lock().map_err(|_| ConventionsError::Storage("database lock poisoned".into()))
    }
}

fn current_version(conn: &Connection) -> Result<usize, ConventionsError> {
    conn.query_row("SELECT COALESCE(MAX(version), 0) FROM conventions_schema_version", [], |r| r.get::<_, i64>(0))
        .map(|v| v as usize)
        .map_err(storage)
}

fn migrate(conn: &mut Connection) -> Result<(), ConventionsError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS conventions_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied = current_version(&tx)?;
    if applied > MIGRATIONS.len() {
        return Err(ConventionsError::Storage(format!(
            "conventions schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO conventions_schema_version (version) VALUES (?1)", [index as i64 + 1])
            .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn upsert_rulebook(conn: &Connection, rulebook: &Rulebook) -> Result<(), ConventionsError> {
    let json = serde_json::to_string(rulebook).map_err(storage)?;
    conn.execute(
        "INSERT INTO rulebooks (repo, fingerprint, created_at, rulebook_json) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(repo) DO UPDATE SET fingerprint = excluded.fingerprint, created_at = excluded.created_at,
                                         rulebook_json = excluded.rulebook_json",
        params![rulebook.repo, rulebook.fingerprint, rulebook.created_at.to_rfc3339(), json],
    )
    .map_err(storage)?;
    Ok(())
}

impl RulebookStore for SqliteRulebookStore {
    fn load(&self, repo: &str) -> Result<Option<Rulebook>, ConventionsError> {
        let conn = self.lock()?;
        let json: Option<String> = conn
            .query_row("SELECT rulebook_json FROM rulebooks WHERE repo = ?1", [repo], |r| r.get(0))
            .optional()
            .map_err(storage)?;
        json.map(|j| serde_json::from_str(&j).map_err(|e| storage(format!("stored rulebook is unreadable: {e}"))))
            .transpose()
    }

    fn save(&self, rulebook: &Rulebook) -> Result<(), ConventionsError> {
        upsert_rulebook(&*self.lock()?, rulebook)
    }

    fn save_with_decisions(&self, rulebook: &Rulebook, decisions: &[RuleDecision]) -> Result<(), ConventionsError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(storage)?;
        upsert_rulebook(&tx, rulebook)?;
        for decision in decisions {
            tx.execute(
                "INSERT INTO rule_decisions (repo, normalized_title, status, decided_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(repo, normalized_title) DO UPDATE SET status = excluded.status, decided_at = excluded.decided_at",
                params![rulebook.repo, decision.normalized_title, decision.status.as_str(), decision.decided_at.to_rfc3339()],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }

    fn decisions(&self, repo: &str) -> Result<HashMap<String, RuleStatus>, ConventionsError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare_cached("SELECT normalized_title, status FROM rule_decisions WHERE repo = ?1")
            .map_err(storage)?;
        let rows = stmt.query_map([repo], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(storage)?;
        let mut out = HashMap::new();
        for row in rows {
            let (title, status) = row.map_err(storage)?;
            // An unknown status (written by a newer qgate) is ignored rather than failing the build.
            if let Some(status) = RuleStatus::parse(&status) {
                out.insert(title, status);
            }
        }
        Ok(out)
    }

    fn repos(&self) -> Result<Vec<String>, ConventionsError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached("SELECT repo FROM rulebooks ORDER BY repo").map_err(storage)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(storage)?;
        rows.map(|r| r.map_err(storage)).collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::conventions::domain::rulebook::{FileLine, Rule, RuleKind};

    fn rulebook(repo: &str) -> Rulebook {
        Rulebook {
            repo: repo.into(),
            fingerprint: "fp".into(),
            rules: vec![Rule {
                id: "R1".into(),
                title: "Domain is pure".into(),
                description: "No IO in domain.".into(),
                kind: RuleKind::Explicit,
                status: RuleStatus::Approved,
                scope_globs: vec!["src/**".into()],
                examples: vec![FileLine { file: "CLAUDE.md".into(), line: 3, quote: "domain is pure".into() }],
                lint_suggestion: Some("deny imports".into()),
            }],
            created_at: DateTime::parse_from_rfc3339("2026-11-01T10:00:00Z").unwrap().with_timezone(&Utc),
        }
    }

    #[test]
    fn round_trips_rulebooks_and_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/qgate.db");
        let store = SqliteRulebookStore::open(&path).unwrap();
        assert_eq!(store.load("/repos/a").unwrap(), None);
        store.save(&rulebook("/repos/b")).unwrap();
        store.save(&rulebook("/repos/a")).unwrap();
        let mut updated = rulebook("/repos/a");
        updated.fingerprint = "fp2".into();
        let decision = RuleDecision {
            normalized_title: "domain is pure".into(),
            status: RuleStatus::Rejected,
            decided_at: Utc::now(),
        };
        store.save_with_decision(&updated, &decision).unwrap();
        store.save_with_decision(&updated, &RuleDecision { status: RuleStatus::Approved, ..decision }).unwrap();
        drop(store);

        let reopened = SqliteRulebookStore::open(&path).unwrap();
        assert_eq!(reopened.load("/repos/a").unwrap(), Some(updated));
        assert_eq!(reopened.repos().unwrap(), ["/repos/a", "/repos/b"]);
        assert_eq!(
            reopened.decisions("/repos/a").unwrap(),
            HashMap::from([("domain is pure".into(), RuleStatus::Approved)])
        );
        assert!(reopened.decisions("/repos/b").unwrap().is_empty());
        assert_eq!(current_version(&reopened.lock().unwrap()).unwrap(), MIGRATIONS.len());
    }

    #[test]
    fn rejects_newer_schema_and_unreadable_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let store = SqliteRulebookStore::open(&path).unwrap();
        {
            let conn = store.lock().unwrap();
            conn.execute("INSERT INTO rulebooks VALUES ('r', 'f', 'now', 'not json')", []).unwrap();
            conn.execute("INSERT INTO rule_decisions VALUES ('r', 't', 'weird', 'now')", []).unwrap();
            conn.execute("INSERT INTO conventions_schema_version VALUES (99)", []).unwrap();
        }
        assert!(matches!(store.load("r"), Err(ConventionsError::Storage(_))));
        assert!(store.decisions("r").unwrap().is_empty());
        drop(store);
        assert!(SqliteRulebookStore::open(&path).is_err());
    }
}
