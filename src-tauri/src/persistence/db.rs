use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{AppError, AppResult};

/// Ordered schema migrations; index + 1 == `PRAGMA user_version` after applying.
/// Never edit a shipped migration, only append new ones.
const MIGRATIONS: &[&str] = &[
    // v1: initial schema
    r#"
    CREATE TABLE settings (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT    NOT NULL
    );

    CREATE TABLE blocklist (
        key         TEXT PRIMARY KEY,
        group_id    TEXT    NOT NULL,
        title       TEXT,
        artist      TEXT,
        source_url  TEXT,
        blocked_by  TEXT    NOT NULL,
        created_at  INTEGER NOT NULL
    );
    CREATE INDEX blocklist_group ON blocklist (group_id);

    -- Position 0 is the interrupted "now playing" item when the app exited mid-track.
    CREATE TABLE queue (
        queue_id       TEXT PRIMARY KEY,
        position       INTEGER NOT NULL,
        track_json     TEXT    NOT NULL,
        requester_json TEXT    NOT NULL,
        added_at       INTEGER NOT NULL
    );
    CREATE INDEX queue_position ON queue (position);

    CREATE TABLE history (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        track_key      TEXT    NOT NULL,
        track_json     TEXT    NOT NULL,
        requester_json TEXT    NOT NULL,
        played_at      INTEGER NOT NULL,
        skipped        INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX history_played_at ON history (played_at);

    CREATE TABLE kv (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    // v2: local library and playlists
    r#"
    CREATE TABLE library_tracks (
        id          INTEGER PRIMARY KEY,
        path        TEXT    NOT NULL UNIQUE,
        title       TEXT    NOT NULL,
        artist      TEXT    NOT NULL DEFAULT '',
        album       TEXT,
        duration_ms INTEGER NOT NULL DEFAULT 0,
        has_art     INTEGER NOT NULL DEFAULT 0,
        file_size   INTEGER NOT NULL,
        modified_ms INTEGER NOT NULL,
        search      TEXT    NOT NULL
    );

    CREATE TABLE playlists (
        id               INTEGER PRIMARY KEY,
        name             TEXT    NOT NULL,
        created_at       INTEGER NOT NULL,
        updated_at       INTEGER NOT NULL,
        source_kind      TEXT,
        source_ref       TEXT,
        source_synced_at INTEGER
    );

    CREATE TABLE playlist_items (
        id          INTEGER PRIMARY KEY,
        playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
        position    INTEGER NOT NULL,
        track_json  TEXT    NOT NULL,
        added_at    INTEGER NOT NULL
    );
    CREATE INDEX playlist_items_order ON playlist_items (playlist_id, position);
    "#,
];

/// Thread-safe handle to the application database. All queries run on
/// Tokio's blocking pool so they never stall the async runtime.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> AppResult<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        migrate(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> AppResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        migrate(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Runs `f` with exclusive access to the connection on the blocking pool.
    pub async fn call<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&mut Connection) -> AppResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| AppError::Internal("database lock poisoned".into()))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| AppError::Internal(format!("database task failed: {e}")))?
    }

    /// Synchronous variant for shutdown paths where no runtime is available.
    pub fn call_blocking<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&mut Connection) -> AppResult<R>,
    {
        let mut guard = self.conn.lock().map_err(|_| AppError::Internal("database lock poisoned".into()))?;
        f(&mut guard)
    }
}

fn migrate(conn: &mut Connection) -> AppResult<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let current = usize::try_from(current).unwrap_or(0);
    if current > MIGRATIONS.len() {
        return Err(AppError::Internal(format!(
            "database schema v{current} is newer than this build supports (v{}); refusing to open",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
        tracing::info!(version = index + 1, "applied database migration");
    }
    Ok(())
}
