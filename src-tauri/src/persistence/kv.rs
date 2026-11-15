use rusqlite::{params, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::Database;
use crate::error::AppResult;

impl Database {
    pub async fn kv_get<T: DeserializeOwned + Send + 'static>(&self, key: &str) -> AppResult<Option<T>> {
        let key = key.to_owned();
        let raw: Option<String> = self
            .call(move |c| Ok(c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()?))
            .await?;
        raw.map(|s| serde_json::from_str(&s).map_err(Into::into)).transpose()
    }

    pub async fn kv_set<T: Serialize>(&self, key: &str, value: &T) -> AppResult<()> {
        let key = key.to_owned();
        let json = serde_json::to_string(value)?;
        self.call(move |c| {
            c.execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, json],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn kv_delete(&self, key: &str) -> AppResult<()> {
        let key = key.to_owned();
        self.call(move |c| {
            c.execute("DELETE FROM kv WHERE key = ?1", [key])?;
            Ok(())
        })
        .await
    }
}
