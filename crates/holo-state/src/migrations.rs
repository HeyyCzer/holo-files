use rusqlite::Connection;

/// Append-only. Never edit a released migration; add a new one.
const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE accounts(
        id           TEXT PRIMARY KEY,
        provider     TEXT NOT NULL,
        remote_id    TEXT NOT NULL,
        display_name TEXT NOT NULL,
        email        TEXT,
        created_at   INTEGER NOT NULL
    );
    CREATE TABLE roots(
        id             TEXT PRIMARY KEY,
        account_id     TEXT NOT NULL REFERENCES accounts(id),
        local_path     TEXT NOT NULL UNIQUE COLLATE NOCASE,
        remote_root_id TEXT NOT NULL,
        remote_path    TEXT NOT NULL,
        enabled        INTEGER NOT NULL DEFAULT 1,
        cursor         TEXT,
        created_at     INTEGER NOT NULL
    );
    CREATE TABLE items(
        root_id   TEXT NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
        remote_id TEXT NOT NULL,
        parent_id TEXT,
        name      TEXT NOT NULL,
        rel_path  TEXT NOT NULL,
        rel_key   TEXT NOT NULL,
        is_dir    INTEGER NOT NULL,
        size      INTEGER NOT NULL,
        modified  INTEGER NOT NULL,
        etag      TEXT,
        ctag      TEXT,
        hash      TEXT,
        synced_at INTEGER NOT NULL,
        PRIMARY KEY(root_id, remote_id)
    );
    CREATE INDEX items_by_path   ON items(root_id, rel_key);
    CREATE INDEX items_by_parent ON items(root_id, parent_id);
    CREATE TABLE ops(
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        root_id    TEXT NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,
        payload    TEXT NOT NULL,
        state      TEXT NOT NULL,
        attempts   INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        not_before INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE INDEX ops_ready ON ops(root_id, state, not_before);
    CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    "#,
    // v2: recent activity shown in the UI (bounded per root).
    r#"
    CREATE TABLE activity(
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        root_id TEXT NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
        at      INTEGER NOT NULL,
        kind    TEXT NOT NULL,
        path    TEXT NOT NULL,
        detail  TEXT,
        bytes   INTEGER,
        error   TEXT
    );
    CREATE INDEX activity_by_root ON activity(root_id, id);
    "#,
];

pub fn apply(conn: &Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = i as i64 + 1;
        conn.execute_batch(&format!(
            "BEGIN; {sql}; PRAGMA user_version = {version}; COMMIT;"
        ))?;
    }
    Ok(())
}

pub const LATEST: i64 = MIGRATIONS.len() as i64;
