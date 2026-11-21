//! SQLite `DesignCache`. Shares the qgate database file but owns its tables
//! (`design_schema_version`, `figma_cache`).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use futures::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, params};

use crate::design::application::{CachedDesign, DesignCache, DesignError, DesignSpec, FetchedDesign, FigmaLink};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Ordered schema migrations; index + 1 is the schema version.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE figma_cache (
        file_key   TEXT NOT NULL,
        node_id    TEXT NOT NULL,
        version    TEXT NOT NULL,
        fetched_at TEXT NOT NULL,
        spec_json  TEXT NOT NULL,
        png        BLOB,
        PRIMARY KEY (file_key, node_id)
    );
"];

#[derive(Clone)]
pub struct SqliteDesignCache {
    conn: Arc<Mutex<Connection>>,
}

fn storage(err: impl std::fmt::Display) -> DesignError {
    DesignError::Storage(err.to_string())
}

impl SqliteDesignCache {
    /// Opens (creating the parent directory and file if needed) `<data_dir>/qgate.db` and migrates.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DesignError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(storage)?;
        }
        let mut conn = Connection::open(path).map_err(storage)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)).map_err(storage)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(storage)?;
        migrate(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    async fn blocking<T, F>(&self, work: F) -> Result<T, DesignError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, DesignError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().map_err(|_| DesignError::Storage("design cache lock poisoned".into()))?;
            work(&guard)
        })
        .await
        .map_err(|e| DesignError::Storage(format!("design cache worker failed: {e}")))?
    }
}

fn migrate(conn: &mut Connection) -> Result<(), DesignError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS design_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied: i64 = tx
        .query_row("SELECT COALESCE(MAX(version), 0) FROM design_schema_version", [], |r| r.get(0))
        .map_err(storage)?;
    let applied = usize::try_from(applied).unwrap_or(usize::MAX);
    if applied > MIGRATIONS.len() {
        return Err(DesignError::Storage(format!(
            "design schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO design_schema_version (version) VALUES (?1)", [index as i64 + 1]).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

fn read(conn: &Connection, file_key: &str, node_id: &str) -> Result<Option<CachedDesign>, DesignError> {
    let row = conn
        .query_row(
            "SELECT version, fetched_at, spec_json, png FROM figma_cache WHERE file_key = ?1 AND node_id = ?2",
            params![file_key, node_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((version, fetched_at, spec_json, png)) = row else { return Ok(None) };
    // An unreadable row (older format) is a miss, not an error: it will be overwritten.
    Ok(serde_json::from_str::<DesignSpec>(&spec_json).ok().map(|spec| CachedDesign { version, fetched_at, spec, png }))
}

impl DesignCache for SqliteDesignCache {
    fn get<'a>(&'a self, link: &'a FigmaLink) -> BoxFuture<'a, Result<Option<CachedDesign>, DesignError>> {
        let (file_key, node_id) = (link.file_key.clone(), link.node_id.clone());
        Box::pin(self.blocking(move |conn| read(conn, &file_key, &node_id)))
    }

    fn put<'a>(&'a self, link: &'a FigmaLink, design: &'a FetchedDesign) -> BoxFuture<'a, Result<(), DesignError>> {
        let (file_key, node_id) = (link.file_key.clone(), link.node_id.clone());
        let version = design.version.clone();
        let png = design.png.clone();
        let spec_json = serde_json::to_string(&design.spec).map_err(storage);
        Box::pin(async move {
            let spec_json = spec_json?;
            let fetched_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            self.blocking(move |conn| {
                conn.execute(
                    "INSERT INTO figma_cache (file_key, node_id, version, fetched_at, spec_json, png)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (file_key, node_id) DO UPDATE SET
                        version = excluded.version, fetched_at = excluded.fetched_at,
                        spec_json = excluded.spec_json, png = excluded.png",
                    params![file_key, node_id, version, fetched_at, spec_json, png],
                )
                .map(|_| ())
                .map_err(storage)
            })
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> FigmaLink {
        FigmaLink { file_key: "File1".into(), node_id: "12:345".into() }
    }

    fn fetched(version: &str, text: &str, png: Option<Vec<u8>>) -> FetchedDesign {
        let spec = DesignSpec { frame_name: "F".into(), texts: vec![text.into()], ..DesignSpec::default() };
        FetchedDesign { spec, version: version.into(), png, notes: vec!["not stored".into()] }
    }

    #[tokio::test]
    async fn stores_and_refreshes_by_version() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SqliteDesignCache::open(dir.path().join("data/qgate.db")).unwrap();
        assert!(cache.get(&link()).await.unwrap().is_none());
        cache.put(&link(), &fetched("1", "old", Some(vec![1, 2]))).await.unwrap();
        let hit = cache.get(&link()).await.unwrap().unwrap();
        assert_eq!((hit.version.as_str(), hit.spec.texts[0].as_str()), ("1", "old"));
        assert_eq!(hit.png, Some(vec![1, 2]));
        assert!(!hit.fetched_at.is_empty());
        cache.put(&link(), &fetched("2", "new", None)).await.unwrap();
        let hit = cache.get(&link()).await.unwrap().unwrap();
        assert_eq!((hit.version.as_str(), hit.spec.texts[0].as_str(), hit.png), ("2", "new", None));
    }

    #[tokio::test]
    async fn reopening_keeps_data_and_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        SqliteDesignCache::open(&path).unwrap().put(&link(), &fetched("1", "kept", None)).await.unwrap();
        let reopened = SqliteDesignCache::open(&path).unwrap();
        assert_eq!(reopened.get(&link()).await.unwrap().unwrap().spec.texts, vec!["kept"]);
        let conn = Connection::open(&path).unwrap();
        conn.execute("INSERT INTO design_schema_version (version) VALUES (99)", []).unwrap();
        assert!(matches!(SqliteDesignCache::open(&path), Err(DesignError::Storage(m)) if m.contains("newer")));
    }

    #[tokio::test]
    async fn corrupt_row_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let cache = SqliteDesignCache::open(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("INSERT INTO figma_cache VALUES ('File1', '12:345', '1', 'now', 'not json', NULL)", [])
            .unwrap();
        assert!(cache.get(&link()).await.unwrap().is_none());
    }
}
