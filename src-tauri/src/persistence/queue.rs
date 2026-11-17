use rusqlite::{params, Connection};

use super::Database;
use crate::error::AppResult;
use crate::model::{QueueItem, Requester, Track};
use crate::util::now_ms;

fn write_queue(conn: &mut Connection, items: &[QueueItem]) -> AppResult<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM queue", [])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO queue (queue_id, position, track_json, requester_json, added_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (position, item) in items.iter().enumerate() {
            stmt.execute(params![
                item.queue_id,
                position as i64,
                serde_json::to_string(&item.track)?,
                serde_json::to_string(&item.requester)?,
                item.added_at_ms,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

impl Database {
    /// Loads the persisted queue in order. Rows that no longer deserialize
    /// (e.g. after a model change) are skipped rather than failing startup.
    pub async fn load_queue(&self) -> AppResult<Vec<QueueItem>> {
        self.call(|c| {
            let mut stmt = c.prepare(
                "SELECT queue_id, track_json, requester_json, added_at FROM queue ORDER BY position ASC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
            })?;
            let mut items = Vec::new();
            for row in rows {
                let (queue_id, track_json, requester_json, added_at_ms) = row?;
                match (serde_json::from_str::<Track>(&track_json), serde_json::from_str::<Requester>(&requester_json)) {
                    (Ok(track), Ok(requester)) => items.push(QueueItem { queue_id, track, requester, added_at_ms, autoplay: false }),
                    _ => tracing::warn!(%queue_id, "dropping unreadable persisted queue row"),
                }
            }
            Ok(items)
        })
        .await
    }

    /// Replaces the persisted queue (current item first, if any).
    pub async fn save_queue(&self, items: Vec<QueueItem>) -> AppResult<()> {
        self.call(move |c| write_queue(c, &items)).await
    }

    pub fn save_queue_blocking(&self, items: &[QueueItem]) -> AppResult<()> {
        self.call_blocking(|c| write_queue(c, items))
    }

    pub async fn push_history(&self, item: QueueItem, skipped: bool) -> AppResult<()> {
        self.call(move |c| {
            c.execute(
                "INSERT INTO history (track_key, track_json, requester_json, played_at, skipped) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    item.track.key,
                    serde_json::to_string(&item.track)?,
                    serde_json::to_string(&item.requester)?,
                    now_ms(),
                    skipped as i64
                ],
            )?;
            // Keep history bounded.
            c.execute(
                "DELETE FROM history WHERE id <= (SELECT id FROM history ORDER BY id DESC LIMIT 1 OFFSET 5000)",
                [],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrackOrigin;

    fn item(id: &str) -> QueueItem {
        QueueItem {
            queue_id: id.into(),
            track: Track {
                key: format!("yt:{id}"),
                origin: TrackOrigin::YouTube { video_id: id.into() },
                title: "t".into(),
                artist: "a".into(),
                album: None,
                artwork_url: None,
                duration_ms: 1000,
                source_url: "u".into(),
            },
            requester: Requester::local(),
            added_at_ms: 1,
            autoplay: false,
        }
    }

    #[tokio::test]
    async fn queue_roundtrip_preserves_order() {
        let db = Database::open_in_memory().unwrap();
        db.save_queue(vec![item("b"), item("a"), item("c")]).await.unwrap();
        let ids: Vec<_> = db.load_queue().await.unwrap().into_iter().map(|i| i.queue_id).collect();
        assert_eq!(ids, ["b", "a", "c"]);
        db.save_queue(vec![item("z")]).await.unwrap();
        assert_eq!(db.load_queue().await.unwrap().len(), 1);
    }
}
