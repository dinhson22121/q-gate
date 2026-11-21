//! `ResultCache` backed by SQLite, in qgate's database (`<data_dir>/qgate.db`). Owns the tables
//! `result_cache_schema_version` and `result_cache`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::cache::application::{CacheError, ContentKey, ResultCache};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Ordered schema migrations; index + 1 is the schema version.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE result_cache (
        fingerprint  TEXT    NOT NULL PRIMARY KEY,
        kind         TEXT    NOT NULL,
        value        BLOB    NOT NULL,
        size_bytes   INTEGER NOT NULL,
        created_at   INTEGER NOT NULL,
        last_used_at INTEGER NOT NULL
    );
    CREATE INDEX idx_result_cache_kind ON result_cache (kind);
    CREATE INDEX idx_result_cache_last_used ON result_cache (last_used_at);
"];

/// A content-addressed result cache. Cheap to clone (shared connection).
#[derive(Clone)]
pub struct SqliteResultCache {
    conn: Arc<Mutex<Connection>>,
}

fn storage(err: impl std::fmt::Display) -> CacheError {
    CacheError(err.to_string())
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl SqliteResultCache {
    /// Opens (creating parent directories and the file if needed) and migrates the cache tables.
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
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CacheError> {
        self.conn.lock().map_err(|_| CacheError("result cache lock poisoned".into()))
    }
}

fn migrate(conn: &mut Connection) -> Result<(), CacheError> {
    let tx = conn.transaction().map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS result_cache_schema_version (version INTEGER NOT NULL PRIMARY KEY)")
        .map_err(storage)?;
    let applied: i64 = tx
        .query_row("SELECT COALESCE(MAX(version), 0) FROM result_cache_schema_version", [], |r| r.get(0))
        .map_err(storage)?;
    let applied = usize::try_from(applied).map_err(storage)?;
    if applied > MIGRATIONS.len() {
        return Err(CacheError(format!(
            "result cache schema version {applied} is newer than this qgate supports ({})",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql).map_err(storage)?;
        tx.execute("INSERT INTO result_cache_schema_version (version) VALUES (?1)", [index as i64 + 1])
            .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

impl ResultCache for SqliteResultCache {
    fn get(&self, key: &ContentKey) -> Result<Option<Vec<u8>>, CacheError> {
        let fingerprint = key.fingerprint();
        let conn = self.lock()?;
        let value: Option<Vec<u8>> = conn
            .query_row("SELECT value FROM result_cache WHERE fingerprint = ?1", [&fingerprint], |r| r.get(0))
            .optional()
            .map_err(storage)?;
        if value.is_some() {
            conn.execute(
                "UPDATE result_cache SET last_used_at = ?1 WHERE fingerprint = ?2",
                params![now_millis(), fingerprint],
            )
            .map_err(storage)?;
        }
        Ok(value)
    }

    fn put(&self, key: &ContentKey, value: &[u8]) -> Result<(), CacheError> {
        let fingerprint = key.fingerprint();
        let now = now_millis();
        let size = i64::try_from(value.len()).map_err(storage)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO result_cache (fingerprint, kind, value, size_bytes, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(fingerprint) DO UPDATE SET
                kind = ?2, value = ?3, size_bytes = ?4, last_used_at = ?5",
            params![fingerprint, key.kind(), value, size, now],
        )
        .map_err(storage)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(kind: &str, hash: &str) -> ContentKey {
        ContentKey::new(kind, 1).with_input("blob", hash)
    }

    #[test]
    fn get_miss_then_put_then_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SqliteResultCache::open(dir.path().join("nested/qgate.db")).unwrap();
        let k = key("review.findings", "oid-1");

        assert_eq!(cache.get(&k).unwrap(), None);
        cache.put(&k, b"payload").unwrap();
        assert_eq!(cache.get(&k).unwrap().as_deref(), Some(b"payload".as_slice()));
    }

    #[test]
    fn put_overwrites_an_existing_payload() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SqliteResultCache::open(dir.path().join("qgate.db")).unwrap();
        let k = key("review.findings", "oid-1");

        cache.put(&k, b"first").unwrap();
        cache.put(&k, b"second").unwrap();
        assert_eq!(cache.get(&k).unwrap().as_deref(), Some(b"second".as_slice()));

        let count: i64 =
            cache.lock().unwrap().query_row("SELECT COUNT(*) FROM result_cache", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "the same fingerprint keeps a single row");
    }

    #[test]
    fn different_inputs_are_distinct_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SqliteResultCache::open(dir.path().join("qgate.db")).unwrap();
        let a = key("review.findings", "oid-a");
        let b = key("review.findings", "oid-b");

        cache.put(&a, b"A").unwrap();
        cache.put(&b, b"B").unwrap();
        assert_eq!(cache.get(&a).unwrap().as_deref(), Some(b"A".as_slice()));
        assert_eq!(cache.get(&b).unwrap().as_deref(), Some(b"B".as_slice()));
    }

    #[test]
    fn stores_kind_and_size_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let k = key("tests.scenarios", "oid-1");
        {
            let cache = SqliteResultCache::open(&path).unwrap();
            cache.put(&k, b"abcd").unwrap();
            let (kind, size): (String, i64) = cache
                .lock()
                .unwrap()
                .query_row("SELECT kind, size_bytes FROM result_cache WHERE fingerprint = ?1", [k.fingerprint()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(kind, "tests.scenarios");
            assert_eq!(size, 4);
        }
        let reopened = SqliteResultCache::open(&path).unwrap();
        assert_eq!(reopened.get(&k).unwrap().as_deref(), Some(b"abcd".as_slice()));
    }

    #[test]
    fn get_refreshes_last_used_at() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SqliteResultCache::open(dir.path().join("qgate.db")).unwrap();
        let k = key("review.findings", "oid-1");
        cache.put(&k, b"x").unwrap();
        let read_at = |c: &SqliteResultCache| -> i64 {
            c.lock().unwrap().query_row("SELECT last_used_at FROM result_cache", [], |r| r.get(0)).unwrap()
        };
        let before = read_at(&cache);
        cache.lock().unwrap().execute("UPDATE result_cache SET last_used_at = 0", []).unwrap();
        cache.get(&k).unwrap();
        assert!(read_at(&cache) >= before, "last_used_at advances on a hit");
    }

    #[test]
    fn rejects_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.db");
        let cache = SqliteResultCache::open(&path).unwrap();
        cache.lock().unwrap().execute("INSERT INTO result_cache_schema_version VALUES (99)", []).unwrap();
        drop(cache);
        assert!(SqliteResultCache::open(&path).err().unwrap().0.contains("newer"));
    }
}
