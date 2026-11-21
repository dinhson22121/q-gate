//! SQLite `ProposalStore`: one row per proposal in `qgate.db` (its own table), the proposal as JSON and
//! the status in a column so a status change is a conditional update.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::policy::application::{PolicyError, ProposalStore};
use crate::policy::domain::{Proposal, ProposalStatus};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Ordered migrations; index + 1 is the schema version.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE policy_proposals (
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        status           TEXT    NOT NULL,
        created_at       TEXT    NOT NULL,
        expires_at       TEXT    NOT NULL,
        repo             TEXT,
        settings_version INTEGER NOT NULL,
        proposal_json    TEXT    NOT NULL
    );
"];

fn storage(err: impl std::fmt::Display) -> PolicyError {
    PolicyError::Storage(format!("policy storage failed: {err}"))
}

pub struct SqliteProposalStore {
    conn: Mutex<Connection>,
}

impl SqliteProposalStore {
    /// Opens (creating parent directories and the file if needed) and migrates `<data_dir>/qgate.db`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PolicyError> {
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

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, PolicyError> {
        self.conn.lock().map_err(|_| PolicyError::Storage("database lock poisoned".into()))
    }
}

fn migrate(conn: &mut Connection) -> Result<(), PolicyError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS policy_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied: i64 = tx
        .query_row("SELECT COALESCE(MAX(version), 0) FROM policy_schema_version", [], |r| r.get(0))
        .map_err(storage)?;
    let applied = usize::try_from(applied).map_err(storage)?;
    if applied > MIGRATIONS.len() {
        return Err(PolicyError::Storage(format!(
            "policy schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO policy_schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn to_json(proposal: &Proposal) -> Result<String, PolicyError> {
    serde_json::to_string(proposal).map_err(storage)
}

/// The row's id and status win over the JSON copy.
fn read_row(id: i64, status: &str, json: &str) -> Result<Proposal, PolicyError> {
    let proposal: Proposal =
        serde_json::from_str(json).map_err(|e| storage(format!("stored proposal {id} is unreadable: {e}")))?;
    let status = ProposalStatus::parse(status).ok_or_else(|| storage(format!("unknown proposal status {status}")))?;
    Ok(Proposal { id, status, ..proposal })
}

impl ProposalStore for SqliteProposalStore {
    fn insert(&self, proposal: &Proposal) -> Result<i64, PolicyError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO policy_proposals (status, created_at, expires_at, repo, settings_version, proposal_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                proposal.status.as_str(),
                proposal.created_at.to_rfc3339(),
                proposal.expires_at.to_rfc3339(),
                proposal.repo,
                proposal.settings_version,
                to_json(proposal)?
            ],
        )
        .map_err(storage)?;
        Ok(conn.last_insert_rowid())
    }

    fn get(&self, id: i64) -> Result<Option<Proposal>, PolicyError> {
        let conn = self.lock()?;
        let row: Option<(String, String)> = conn
            .query_row("SELECT status, proposal_json FROM policy_proposals WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(storage)?;
        row.map(|(status, json)| read_row(id, &status, &json)).transpose()
    }

    fn transition(&self, proposal: &Proposal, from: ProposalStatus) -> Result<bool, PolicyError> {
        let conn = self.lock()?;
        let changed = conn
            .execute(
                "UPDATE policy_proposals SET status = ?1, proposal_json = ?2 WHERE id = ?3 AND status = ?4",
                params![proposal.status.as_str(), to_json(proposal)?, proposal.id, from.as_str()],
            )
            .map_err(storage)?;
        Ok(changed == 1)
    }

    fn recent(&self, limit: u32) -> Result<Vec<Proposal>, PolicyError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare_cached("SELECT id, status, proposal_json FROM policy_proposals ORDER BY id DESC LIMIT ?1")
            .map_err(storage)?;
        let rows = stmt
            .query_map([limit], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(storage)?;
        rows.map(|row| {
            let (id, status, json) = row.map_err(storage)?;
            read_row(id, &status, &json)
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::policy::domain::{ProposalContent, ProposalDraft};

    fn proposal(summary: &str) -> Proposal {
        let now = DateTime::parse_from_rfc3339("2026-11-21T10:00:00Z").unwrap().with_timezone(&Utc);
        let content = ProposalContent { summary: summary.into(), ..ProposalContent::default() };
        Proposal::new("desc".into(), Some("/repos/a".into()), 4, ProposalDraft::default(), content, now)
    }

    #[test]
    fn round_trips_and_transitions_only_from_the_expected_status() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/qgate.db");
        let store = SqliteProposalStore::open(&path).unwrap();
        assert_eq!(store.get(1).unwrap(), None);
        let first = store.insert(&proposal("one")).unwrap();
        let second = store.insert(&proposal("two")).unwrap();
        assert_eq!((first, second), (1, 2));
        let loaded = store.get(first).unwrap().unwrap();
        assert_eq!(loaded, Proposal { id: 1, ..proposal("one") });

        let applied = Proposal { status: ProposalStatus::Applied, ..loaded };
        assert!(store.transition(&applied, ProposalStatus::Draft).unwrap());
        assert!(!store.transition(&applied, ProposalStatus::Draft).unwrap(), "already applied");
        drop(store);

        let reopened = SqliteProposalStore::open(&path).unwrap();
        assert_eq!(reopened.get(1).unwrap().unwrap().status, ProposalStatus::Applied);
        let recent = reopened.recent(10).unwrap();
        assert_eq!(recent.iter().map(|p| p.id).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(reopened.recent(1).unwrap().len(), 1);
    }

    #[test]
    fn unreadable_rows_and_newer_schemas_are_storage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let store = SqliteProposalStore::open(&path).unwrap();
        store.insert(&proposal("x")).unwrap();
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE policy_proposals SET proposal_json = '{'", []).unwrap();
            conn.execute("INSERT INTO policy_schema_version (version) VALUES (99)", []).unwrap();
        }
        assert!(matches!(store.get(1), Err(PolicyError::Storage(_))));
        drop(store);
        assert!(matches!(SqliteProposalStore::open(&path), Err(PolicyError::Storage(m)) if m.contains("newer")));
    }
}
