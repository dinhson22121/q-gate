//! SQLite cache for parsed tickets and ticket briefs, in qgate's database (`<data_dir>/qgate.db`).
//! Owns the tables `tickets_schema_version`, `ticket_cache` and `brief_cache`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, params};

use crate::review::application::TicketBrief;
use crate::tickets::application::{BriefCache, CacheError, CachedTicket, TicketCache};
use crate::tickets::domain::TicketKey;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Briefs older than this are deleted when the cache is opened.
const BRIEF_RETENTION_DAYS: i64 = 30;

/// Ordered schema migrations; index + 1 is the schema version.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE ticket_cache (
        key          TEXT    NOT NULL PRIMARY KEY,
        jira_updated TEXT,
        fetched_at   INTEGER NOT NULL,
        ticket_json  TEXT    NOT NULL
    );
    CREATE TABLE brief_cache (
        hash       TEXT    NOT NULL PRIMARY KEY,
        brief_json TEXT    NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX idx_brief_cache_created_at ON brief_cache (created_at);
"];

/// Implements [`TicketCache`] and [`BriefCache`]. Cheap to clone (shared connection).
#[derive(Clone)]
pub struct SqliteTicketCache {
    conn: Arc<Mutex<Connection>>,
}

fn storage(err: impl std::fmt::Display) -> CacheError {
    CacheError(err.to_string())
}

impl SqliteTicketCache {
    /// Opens (creating parent directories and the file if needed), migrates, and prunes old briefs.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CacheError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(storage)?;
        }
        let mut conn = Connection::open(path).map_err(storage)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)).map_err(storage)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(storage)?;
        migrate(&mut conn)?;
        let cutoff = (Utc::now() - chrono::Duration::days(BRIEF_RETENTION_DAYS)).timestamp_millis();
        conn.execute("DELETE FROM brief_cache WHERE created_at < ?1", [cutoff]).map_err(storage)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Runs `work` on the connection in a blocking thread.
    async fn with_conn<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, CacheError> + Send + 'static,
    ) -> Result<T, CacheError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().map_err(|_| CacheError("ticket cache lock poisoned".into()))?;
            work(&guard)
        })
        .await
        .map_err(storage)?
    }
}

fn migrate(conn: &mut Connection) -> Result<(), CacheError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS tickets_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied: i64 = tx
        .query_row("SELECT COALESCE(MAX(version), 0) FROM tickets_schema_version", [], |r| r.get(0))
        .map_err(storage)?;
    let applied = usize::try_from(applied).map_err(storage)?;
    if applied > MIGRATIONS.len() {
        return Err(CacheError(format!(
            "ticket cache schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO tickets_schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn from_millis(ms: i64) -> Result<DateTime<Utc>, CacheError> {
    DateTime::from_timestamp_millis(ms).ok_or_else(|| CacheError(format!("invalid timestamp {ms}")))
}

impl TicketCache for SqliteTicketCache {
    fn get_ticket<'a>(&'a self, key: &'a TicketKey) -> BoxFuture<'a, Result<Option<CachedTicket>, CacheError>> {
        let key = key.to_string();
        Box::pin(self.with_conn(move |conn| {
            let row: Option<(Option<String>, i64, String)> = conn
                .query_row(
                    "SELECT jira_updated, fetched_at, ticket_json FROM ticket_cache WHERE key = ?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(storage)?;
            let Some((jira_updated, fetched_at, json)) = row else { return Ok(None) };
            // An unreadable entry (older format, corruption) is a cache miss, not an error.
            let Ok(ticket) = serde_json::from_str(&json) else { return Ok(None) };
            Ok(Some(CachedTicket { ticket, jira_updated, fetched_at: from_millis(fetched_at)? }))
        }))
    }

    fn put_ticket<'a>(&'a self, entry: &'a CachedTicket) -> BoxFuture<'a, Result<(), CacheError>> {
        let key = entry.ticket.key.to_string();
        let updated = entry.jira_updated.clone();
        let fetched_at = entry.fetched_at.timestamp_millis();
        let json = serde_json::to_string(&entry.ticket).map_err(storage);
        Box::pin(self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ticket_cache (key, jira_updated, fetched_at, ticket_json) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(key) DO UPDATE SET jira_updated = ?2, fetched_at = ?3, ticket_json = ?4",
                params![key, updated, fetched_at, json?],
            )
            .map_err(storage)?;
            Ok(())
        }))
    }
}

impl BriefCache for SqliteTicketCache {
    fn get_brief<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, Result<Option<TicketBrief>, CacheError>> {
        let hash = hash.to_string();
        Box::pin(self.with_conn(move |conn| {
            let json: Option<String> = conn
                .query_row("SELECT brief_json FROM brief_cache WHERE hash = ?1", [&hash], |r| r.get(0))
                .optional()
                .map_err(storage)?;
            Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
        }))
    }

    fn put_brief<'a>(&'a self, hash: &'a str, brief: &'a TicketBrief) -> BoxFuture<'a, Result<(), CacheError>> {
        let hash = hash.to_string();
        let json = serde_json::to_string(brief).map_err(storage);
        let now = Utc::now().timestamp_millis();
        Box::pin(self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO brief_cache (hash, brief_json, created_at) VALUES (?1, ?2, ?3)",
                params![hash, json?, now],
            )
            .map_err(storage)?;
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::application::TestScenario;
    use crate::tickets::domain::Ticket;

    fn entry(updated: &str, minute: u32) -> CachedTicket {
        let mut ticket = Ticket::new(TicketKey::parse("AB-1").unwrap(), "Title");
        ticket.criteria = vec!["a".into()];
        ticket.design_links = vec!["https://www.figma.com/design/K/x".into()];
        let fetched_at = format!("2026-11-20T10:{minute:02}:00Z");
        CachedTicket {
            ticket,
            jira_updated: Some(updated.into()),
            fetched_at: DateTime::parse_from_rfc3339(&fetched_at).unwrap().to_utc(),
        }
    }

    #[tokio::test]
    async fn stores_replaces_and_reads_tickets() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SqliteTicketCache::open(dir.path().join("nested/qgate.db")).unwrap();
        let key = TicketKey::parse("AB-1").unwrap();
        assert_eq!(cache.get_ticket(&key).await.unwrap(), None);

        cache.put_ticket(&entry("v1", 0)).await.unwrap();
        cache.put_ticket(&entry("v2", 5)).await.unwrap();

        assert_eq!(cache.get_ticket(&key).await.unwrap(), Some(entry("v2", 5)));
    }

    #[tokio::test]
    async fn stores_briefs_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let brief = TicketBrief {
            scenarios: vec![TestScenario {
                id: "S1".into(),
                criterion_ids: vec!["AB-1/AC-1".into()],
                title: "t".into(),
                given: "g".into(),
                when: "w".into(),
                then: "th".into(),
            }],
            implicit_requirements: vec!["audit".into()],
            context: "ctx".into(),
        };
        {
            let cache = SqliteTicketCache::open(&path).unwrap();
            assert_eq!(cache.get_brief("h1").await.unwrap(), None);
            cache.put_brief("h1", &brief).await.unwrap();
            cache.put_brief("h1", &brief).await.unwrap();
        }
        let reopened = SqliteTicketCache::open(&path).unwrap();
        assert_eq!(reopened.get_brief("h1").await.unwrap(), Some(brief));
    }

    #[tokio::test]
    async fn unreadable_entries_are_misses_and_old_briefs_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let cache = SqliteTicketCache::open(&path).unwrap();
        {
            let conn = cache.conn.lock().unwrap();
            conn.execute("INSERT INTO ticket_cache VALUES ('AB-1', NULL, 0, 'not json')", []).unwrap();
            conn.execute("INSERT INTO brief_cache VALUES ('bad', '{', ?1)", [Utc::now().timestamp_millis()]).unwrap();
            conn.execute("INSERT INTO brief_cache VALUES ('old', '{}', 0)", []).unwrap();
        }
        assert_eq!(cache.get_ticket(&TicketKey::parse("AB-1").unwrap()).await.unwrap(), None);
        assert_eq!(cache.get_brief("bad").await.unwrap(), None);
        drop(cache);

        let reopened = SqliteTicketCache::open(&path).unwrap();
        let count: i64 =
            reopened.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM brief_cache", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn rejects_newer_schema_and_shares_file_with_other_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER); INSERT INTO schema_version VALUES (7);")
                .unwrap();
        }
        let cache = SqliteTicketCache::open(&path).unwrap();
        let mode: String = cache.conn.lock().unwrap().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        cache.conn.lock().unwrap().execute("INSERT INTO tickets_schema_version VALUES (99)", []).unwrap();
        drop(cache);
        assert!(SqliteTicketCache::open(&path).err().unwrap().0.contains("newer"));
    }
}
