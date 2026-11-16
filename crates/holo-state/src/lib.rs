//! Durable sync state in SQLite (WAL).
//!
//! `items` holds the **last synchronized** view of each remote item (the "base" for
//! three-way comparison). `ops` is the operation journal that makes local→remote work
//! resumable after a crash.

mod migrations;
mod records;

use std::{path::Path, sync::Mutex};

use chrono::{DateTime, Utc};
use holo_core::{ContentHash, RelPath, RemoteItemId};
use rusqlite::{Connection, OptionalExtension, params};

pub use migrations::LATEST as SCHEMA_VERSION;
pub use records::*;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
}

pub type StateResult<T> = Result<T, StateError>;

/// Activity entries kept per root.
pub const ACTIVITY_KEEP: i64 = 1000;

/// Thread-safe handle; every call is a short transaction, so a plain mutex is enough
/// and keeps Cloud Filter callbacks from ever waiting on long work.
pub struct StateStore {
    conn: Mutex<Connection>,
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}
fn from_ms(v: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(v).unwrap_or_default()
}

impl StateStore {
    pub fn open(path: &Path) -> StateResult<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> StateResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> StateResult<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrations::apply(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> StateResult<T> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        Ok(f(&conn)?)
    }

    /// Run several statements atomically.
    pub fn transaction<T>(&self, f: impl FnOnce(&Tx<'_>) -> StateResult<T>) -> StateResult<T> {
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction()?;
        let out = f(&Tx(&tx))?;
        tx.commit()?;
        Ok(out)
    }

    pub fn schema_version(&self) -> StateResult<i64> {
        self.with(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
    }

    // ---------------------------------------------------------------- accounts

    pub fn upsert_account(&self, a: &AccountRecord) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO accounts(id, provider, remote_id, display_name, email, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET display_name = excluded.display_name, email = excluded.email",
                params![a.id, a.provider.as_str(), a.remote_id, a.display_name, a.email, ms(a.created_at)],
            )
            .map(|_| ())
        })
    }

    pub fn accounts(&self) -> StateResult<Vec<AccountRecord>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, provider, remote_id, display_name, email, created_at FROM accounts ORDER BY id")?;
            s.query_map([], AccountRecord::from_row)?.collect()
        })
    }

    pub fn account(&self, id: &str) -> StateResult<Option<AccountRecord>> {
        self.with(|c| {
            c.query_row(
                "SELECT id, provider, remote_id, display_name, email, created_at FROM accounts WHERE id = ?1",
                [id],
                AccountRecord::from_row,
            )
            .optional()
        })
    }

    pub fn delete_account(&self, id: &str) -> StateResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM accounts WHERE id = ?1", [id])
                .map(|_| ())
        })
    }

    // ------------------------------------------------------------------- roots

    pub fn insert_root(&self, r: &RootRecord) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO roots(id, account_id, local_path, remote_root_id, remote_path, enabled, cursor, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    r.id,
                    r.account_id,
                    r.local_path,
                    r.remote_root_id.as_str(),
                    r.remote_path,
                    r.enabled,
                    r.cursor,
                    ms(r.created_at)
                ],
            )
            .map(|_| ())
        })
    }

    pub fn roots(&self) -> StateResult<Vec<RootRecord>> {
        self.with(|c| {
            let mut s = c.prepare(&format!("{ROOT_SELECT} ORDER BY created_at"))?;
            s.query_map([], RootRecord::from_row)?.collect()
        })
    }

    pub fn root(&self, id: &str) -> StateResult<Option<RootRecord>> {
        self.with(|c| {
            c.query_row(
                &format!("{ROOT_SELECT} WHERE id = ?1"),
                [id],
                RootRecord::from_row,
            )
            .optional()
        })
    }

    pub fn root_by_local_path(&self, path: &str) -> StateResult<Option<RootRecord>> {
        self.with(|c| {
            c.query_row(
                &format!("{ROOT_SELECT} WHERE local_path = ?1"),
                [path],
                RootRecord::from_row,
            )
            .optional()
        })
    }

    pub fn delete_root(&self, id: &str) -> StateResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM roots WHERE id = ?1", [id])
                .map(|_| ())
        })
    }

    pub fn set_root_enabled(&self, id: &str, enabled: bool) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE roots SET enabled = ?2 WHERE id = ?1",
                params![id, enabled],
            )
            .map(|_| ())
        })
    }

    pub fn set_cursor(&self, root_id: &str, cursor: Option<&str>) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE roots SET cursor = ?2 WHERE id = ?1",
                params![root_id, cursor],
            )
            .map(|_| ())
        })
    }

    // ------------------------------------------------------------------- items

    pub fn upsert_item(&self, item: &ItemRecord) -> StateResult<()> {
        self.with(|c| Tx::upsert_item_on(c, item))
    }

    pub fn item(&self, root_id: &str, id: &RemoteItemId) -> StateResult<Option<ItemRecord>> {
        self.with(|c| {
            c.query_row(
                &format!("{ITEM_SELECT} WHERE root_id = ?1 AND remote_id = ?2"),
                params![root_id, id.as_str()],
                ItemRecord::from_row,
            )
            .optional()
        })
    }

    pub fn item_by_path(&self, root_id: &str, path: &RelPath) -> StateResult<Option<ItemRecord>> {
        self.with(|c| {
            c.query_row(
                &format!("{ITEM_SELECT} WHERE root_id = ?1 AND rel_key = ?2"),
                params![root_id, path.key()],
                ItemRecord::from_row,
            )
            .optional()
        })
    }

    pub fn children(&self, root_id: &str, parent: &RemoteItemId) -> StateResult<Vec<ItemRecord>> {
        self.with(|c| {
            let mut s = c.prepare(&format!(
                "{ITEM_SELECT} WHERE root_id = ?1 AND parent_id = ?2"
            ))?;
            s.query_map(params![root_id, parent.as_str()], ItemRecord::from_row)?
                .collect()
        })
    }

    /// All items at or below `prefix`, shallowest first.
    pub fn subtree(&self, root_id: &str, prefix: &RelPath) -> StateResult<Vec<ItemRecord>> {
        self.with(|c| {
            let mut s = c.prepare(&format!(
                "{ITEM_SELECT} WHERE root_id = ?1 AND (?2 = '' OR rel_key = ?2 OR rel_key LIKE ?3 ESCAPE '\\')
                 ORDER BY length(rel_key)"
            ))?;
            let key = prefix.key();
            let like = format!("{}/%", escape_like(&key));
            s.query_map(params![root_id, key, like], ItemRecord::from_row)?.collect()
        })
    }

    pub fn all_items(&self, root_id: &str) -> StateResult<Vec<ItemRecord>> {
        self.subtree(root_id, &RelPath::root())
    }

    pub fn item_count(&self, root_id: &str) -> StateResult<u64> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM items WHERE root_id = ?1",
                [root_id],
                |r| r.get::<_, i64>(0).map(|v| v as u64),
            )
        })
    }

    pub fn delete_item(&self, root_id: &str, id: &RemoteItemId) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "DELETE FROM items WHERE root_id = ?1 AND remote_id = ?2",
                params![root_id, id.as_str()],
            )
            .map(|_| ())
        })
    }

    pub fn delete_subtree(&self, root_id: &str, prefix: &RelPath) -> StateResult<usize> {
        self.transaction(|tx| tx.delete_subtree(root_id, prefix))
    }

    /// Rewrite `rel_path` of a moved folder's descendants.
    pub fn rebase_subtree(
        &self,
        root_id: &str,
        old: &RelPath,
        new: &RelPath,
    ) -> StateResult<usize> {
        self.transaction(|tx| tx.rebase_subtree(root_id, old, new))
    }

    // --------------------------------------------------------------------- ops

    pub fn enqueue_op(&self, root_id: &str, kind: &str, payload: &str) -> StateResult<i64> {
        self.with(|c| {
            let now = ms(Utc::now());
            c.execute(
                "INSERT INTO ops(root_id, kind, payload, state, attempts, not_before, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', 0, 0, ?4, ?4)",
                params![root_id, kind, payload, now],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Oldest runnable pending op for the root.
    pub fn next_op(&self, root_id: &str) -> StateResult<Option<OpRecord>> {
        self.with(|c| {
            c.query_row(
                &format!("{OP_SELECT} WHERE root_id = ?1 AND state = 'pending' AND not_before <= ?2 ORDER BY id LIMIT 1"),
                params![root_id, ms(Utc::now())],
                OpRecord::from_row,
            )
            .optional()
        })
    }

    /// Runnable pending ops for the root, oldest first.
    pub fn due_ops(&self, root_id: &str, limit: usize) -> StateResult<Vec<OpRecord>> {
        self.with(|c| {
            let mut s = c.prepare(&format!(
                "{OP_SELECT} WHERE root_id = ?1 AND state = 'pending' AND not_before <= ?2 ORDER BY id LIMIT ?3"
            ))?;
            s.query_map(
                params![root_id, ms(Utc::now()), limit as i64],
                OpRecord::from_row,
            )?
            .collect()
        })
    }

    /// When the next deferred op becomes runnable, if any.
    pub fn next_op_due(&self, root_id: &str) -> StateResult<Option<DateTime<Utc>>> {
        self.with(|c| {
            c.query_row(
                "SELECT MIN(not_before) FROM ops WHERE root_id = ?1 AND state = 'pending'",
                [root_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .map(|v| v.map(from_ms))
        })
    }

    /// Record something that happened, keeping the newest `ACTIVITY_KEEP` per root.
    /// Repeated downloads of the same file within a few minutes (an app reading it in
    /// chunks) are merged into one entry.
    pub fn add_activity(&self, root_id: &str, a: &NewActivity<'_>) -> StateResult<()> {
        const MERGE_WINDOW_MS: i64 = 5 * 60 * 1000;
        let now = ms(Utc::now());
        self.with(|c| {
            if a.kind == ActivityKind::Downloaded && a.error.is_none() {
                let merged = c.execute(
                    "UPDATE activity SET at = ?3, bytes = COALESCE(bytes, 0) + ?4
                     WHERE id = (SELECT id FROM activity WHERE root_id = ?1 ORDER BY id DESC LIMIT 1)
                       AND kind = 'downloaded' AND path = ?2 AND error IS NULL AND at >= ?5",
                    params![root_id, a.path, now, a.bytes.unwrap_or(0) as i64, now - MERGE_WINDOW_MS],
                )?;
                if merged > 0 {
                    return Ok(());
                }
            }
            c.execute(
                "INSERT INTO activity(root_id, at, kind, path, detail, bytes, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    root_id,
                    now,
                    a.kind.as_str(),
                    a.path,
                    a.detail,
                    a.bytes.map(|b| b as i64),
                    a.error
                ],
            )?;
            c.execute(
                "DELETE FROM activity WHERE root_id = ?1 AND id <= (
                     SELECT id FROM activity WHERE root_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
                params![root_id, ACTIVITY_KEEP],
            )
            .map(|_| ())
        })
    }

    /// Newest first; all roots when `root_id` is `None`.
    pub fn recent_activity(
        &self,
        root_id: Option<&str>,
        limit: usize,
    ) -> StateResult<Vec<ActivityRecord>> {
        self.with(|c| {
            let mut s = c.prepare(
                "SELECT id, root_id, at, kind, path, detail, bytes, error FROM activity
                 WHERE ?1 IS NULL OR root_id = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            s.query_map(params![root_id, limit as i64], ActivityRecord::from_row)?
                .collect()
        })
    }

    pub fn complete_op(&self, id: i64) -> StateResult<()> {
        self.with(|c| c.execute("DELETE FROM ops WHERE id = ?1", [id]).map(|_| ()))
    }

    pub fn retry_op(&self, id: i64, error: &str, not_before: DateTime<Utc>) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE ops SET attempts = attempts + 1, last_error = ?2, not_before = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, error, ms(not_before), ms(Utc::now())],
            )
            .map(|_| ())
        })
    }

    pub fn set_op_state(&self, id: i64, state: OpState, error: Option<&str>) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE ops SET state = ?2, last_error = COALESCE(?3, last_error), updated_at = ?4 WHERE id = ?1",
                params![id, state.as_str(), error, ms(Utc::now())],
            )
            .map(|_| ())
        })
    }

    /// Move all `held` ops back to `pending` (user confirmed a mass delete).
    pub fn release_held_ops(&self, root_id: Option<&str>) -> StateResult<usize> {
        self.with(|c| {
            c.execute(
                "UPDATE ops SET state = 'pending', not_before = 0 WHERE state = 'held' AND (?1 IS NULL OR root_id = ?1)",
                params![root_id],
            )
        })
    }

    /// Drop held ops (user rejected a mass delete).
    pub fn discard_held_ops(&self, root_id: Option<&str>) -> StateResult<usize> {
        self.with(|c| {
            c.execute(
                "DELETE FROM ops WHERE state = 'held' AND (?1 IS NULL OR root_id = ?1)",
                params![root_id],
            )
        })
    }

    pub fn ops(&self, root_id: Option<&str>) -> StateResult<Vec<OpRecord>> {
        self.with(|c| {
            let mut s = c.prepare(&format!(
                "{OP_SELECT} WHERE (?1 IS NULL OR root_id = ?1) ORDER BY id"
            ))?;
            s.query_map(params![root_id], OpRecord::from_row)?.collect()
        })
    }

    pub fn op_counts(&self, root_id: &str) -> StateResult<OpCounts> {
        self.with(|c| {
            c.query_row(
                "SELECT
                   COALESCE(SUM(state = 'pending'), 0),
                   COALESCE(SUM(state = 'held'), 0),
                   COALESCE(SUM(state = 'failed'), 0)
                 FROM ops WHERE root_id = ?1",
                [root_id],
                |r| {
                    Ok(OpCounts {
                        pending: r.get::<_, i64>(0)? as u64,
                        held: r.get::<_, i64>(1)? as u64,
                        failed: r.get::<_, i64>(2)? as u64,
                    })
                },
            )
        })
    }

    // ---------------------------------------------------------------------- kv

    pub fn kv_get(&self, key: &str) -> StateResult<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        })
    }

    pub fn kv_set(&self, key: &str, value: &str) -> StateResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO kv(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }
}

/// Transaction-scoped operations.
pub struct Tx<'a>(&'a rusqlite::Transaction<'a>);

impl Tx<'_> {
    fn upsert_item_on(c: &Connection, i: &ItemRecord) -> rusqlite::Result<()> {
        c.execute(
            "INSERT INTO items(root_id, remote_id, parent_id, name, rel_path, rel_key, is_dir, size, modified, etag, ctag, hash, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(root_id, remote_id) DO UPDATE SET
               parent_id = excluded.parent_id, name = excluded.name, rel_path = excluded.rel_path,
               rel_key = excluded.rel_key, is_dir = excluded.is_dir, size = excluded.size,
               modified = excluded.modified, etag = excluded.etag, ctag = excluded.ctag,
               hash = excluded.hash, synced_at = excluded.synced_at",
            params![
                i.root_id,
                i.remote_id.as_str(),
                i.parent_id.as_ref().map(|p| p.as_str()),
                i.name,
                i.rel_path.as_str(),
                i.rel_path.key(),
                i.is_dir,
                i.size as i64,
                ms(i.modified),
                i.etag,
                i.ctag,
                i.hash.as_ref().map(ContentHash::to_storage),
                ms(i.synced_at),
            ],
        )
        .map(|_| ())
    }

    pub fn upsert_item(&self, item: &ItemRecord) -> StateResult<()> {
        Ok(Self::upsert_item_on(self.0, item)?)
    }

    pub fn delete_item(&self, root_id: &str, id: &RemoteItemId) -> StateResult<()> {
        self.0.execute(
            "DELETE FROM items WHERE root_id = ?1 AND remote_id = ?2",
            params![root_id, id.as_str()],
        )?;
        Ok(())
    }

    pub fn delete_subtree(&self, root_id: &str, prefix: &RelPath) -> StateResult<usize> {
        let key = prefix.key();
        Ok(self.0.execute(
            "DELETE FROM items WHERE root_id = ?1 AND (rel_key = ?2 OR rel_key LIKE ?3 ESCAPE '\\')",
            params![root_id, key, format!("{}/%", escape_like(&key))],
        )?)
    }

    pub fn rebase_subtree(
        &self,
        root_id: &str,
        old: &RelPath,
        new: &RelPath,
    ) -> StateResult<usize> {
        let mut s = self.0.prepare(&format!(
            "{ITEM_SELECT} WHERE root_id = ?1 AND rel_key LIKE ?2 ESCAPE '\\'"
        ))?;
        let rows: Vec<ItemRecord> = s
            .query_map(
                params![root_id, format!("{}/%", escape_like(&old.key()))],
                ItemRecord::from_row,
            )?
            .collect::<rusqlite::Result<_>>()?;
        let mut n = 0;
        for mut r in rows {
            if let Some(p) = r.rel_path.rebase(old, new) {
                r.rel_path = p;
                Self::upsert_item_on(self.0, &r)?;
                n += 1;
            }
        }
        Ok(n)
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

const ROOT_SELECT: &str = "SELECT id, account_id, local_path, remote_root_id, remote_path, enabled, cursor, created_at FROM roots";
const ITEM_SELECT: &str = "SELECT root_id, remote_id, parent_id, name, rel_path, is_dir, size, modified, etag, ctag, hash, synced_at FROM items";
const OP_SELECT: &str = "SELECT id, root_id, kind, payload, state, attempts, last_error, not_before, created_at FROM ops";

#[cfg(test)]
mod tests;
