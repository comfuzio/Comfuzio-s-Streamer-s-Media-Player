use rusqlite::{params, params_from_iter, OptionalExtension, Row};

use super::Database;
use crate::error::AppResult;
use crate::model::BlockEntry;

fn row_to_entry(r: &Row<'_>) -> rusqlite::Result<BlockEntry> {
    Ok(BlockEntry {
        key: r.get(0)?,
        group_id: r.get(1)?,
        title: r.get(2)?,
        artist: r.get(3)?,
        source_url: r.get(4)?,
        blocked_by: r.get(5)?,
        created_at_ms: r.get(6)?,
    })
}

const COLUMNS: &str = "key, group_id, title, artist, source_url, blocked_by, created_at";

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

impl Database {
    pub async fn blocklist_all(&self) -> AppResult<Vec<BlockEntry>> {
        self.call(|c| {
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM blocklist ORDER BY created_at DESC"))?;
            let entries = stmt.query_map([], row_to_entry)?.collect::<Result<Vec<_>, _>>()?;
            Ok(entries)
        })
        .await
    }

    /// Returns the first entry matching any of `keys`.
    pub async fn blocklist_find(&self, keys: Vec<String>) -> AppResult<Option<BlockEntry>> {
        if keys.is_empty() {
            return Ok(None);
        }
        self.call(move |c| {
            let sql = format!("SELECT {COLUMNS} FROM blocklist WHERE key IN ({}) LIMIT 1", placeholders(keys.len()));
            Ok(c.query_row(&sql, params_from_iter(keys.iter()), row_to_entry).optional()?)
        })
        .await
    }

    /// Inserts all entries atomically (upserting existing keys).
    pub async fn blocklist_insert(&self, entries: Vec<BlockEntry>) -> AppResult<()> {
        self.call(move |c| {
            let tx = c.transaction()?;
            {
                let mut stmt = tx.prepare(&format!(
                    "INSERT INTO blocklist ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(key) DO UPDATE SET group_id = excluded.group_id, title = excluded.title,
                        artist = excluded.artist, source_url = excluded.source_url,
                        blocked_by = excluded.blocked_by, created_at = excluded.created_at"
                ))?;
                for e in &entries {
                    stmt.execute(params![
                        e.key,
                        e.group_id,
                        e.title,
                        e.artist,
                        e.source_url,
                        e.blocked_by,
                        e.created_at_ms
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Removes every entry whose key matches, along with the rest of its block
    /// group. Returns the removed entries.
    pub async fn blocklist_remove(&self, keys: Vec<String>) -> AppResult<Vec<BlockEntry>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        self.call(move |c| {
            let tx = c.transaction()?;
            let ph = placeholders(keys.len());
            let removed = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLUMNS} FROM blocklist WHERE group_id IN (SELECT group_id FROM blocklist WHERE key IN ({ph}))"
                ))?;
                let rows = stmt.query_map(params_from_iter(keys.iter()), row_to_entry)?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            tx.execute(
                &format!("DELETE FROM blocklist WHERE group_id IN (SELECT group_id FROM blocklist WHERE key IN ({ph}))"),
                params_from_iter(keys.iter()),
            )?;
            tx.commit()?;
            Ok(removed)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, group: &str) -> BlockEntry {
        BlockEntry {
            key: key.into(),
            group_id: group.into(),
            title: Some("t".into()),
            artist: None,
            source_url: None,
            blocked_by: "mod".into(),
            created_at_ms: 1,
        }
    }

    #[tokio::test]
    async fn unblocking_one_key_lifts_its_group() {
        let db = Database::open_in_memory().unwrap();
        db.blocklist_insert(vec![entry("sp:1", "g1"), entry("yt:1", "g1"), entry("yt:2", "g2")]).await.unwrap();
        assert!(db.blocklist_find(vec!["yt:1".into()]).await.unwrap().is_some());

        let removed = db.blocklist_remove(vec!["sp:1".into()]).await.unwrap();
        assert_eq!(removed.len(), 2);
        assert!(db.blocklist_find(vec!["yt:1".into()]).await.unwrap().is_none());
        assert!(db.blocklist_find(vec!["yt:2".into()]).await.unwrap().is_some());
    }
}
