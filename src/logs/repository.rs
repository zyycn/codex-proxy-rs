use sqlx::{Row, SqlitePool};

use crate::{
    logs::event::{EventLevel, EventLog},
    pagination::{clamp_limit, decode_cursor, encode_cursor, Page},
};

#[derive(Clone)]
pub struct EventLogRepository {
    pool: SqlitePool,
}

impl EventLogRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, event: EventLog) -> Result<(), sqlx::Error> {
        sqlx::query(
            "insert into event_logs (id, kind, level, message, metadata_json, created_at) values (?, ?, ?, ?, '{}', ?)",
        )
        .bind(event.id)
        .bind(event.kind)
        .bind(event.level.as_str())
        .bind(event.message)
        .bind(event.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<EventLog>, sqlx::Error> {
        let limit = clamp_limit(limit);
        let mut rows = if let Some(cursor) = cursor.and_then(|c| decode_cursor(&c)) {
            sqlx::query(
                "select id, kind, level, message, created_at from event_logs where (created_at < ? or (created_at = ? and id < ?)) order by created_at desc, id desc limit ?",
            )
            .bind(&cursor.0)
            .bind(&cursor.0)
            .bind(&cursor.1)
            .bind(i64::from(limit + 1))
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select id, kind, level, message, created_at from event_logs order by created_at desc, id desc limit ?",
            )
            .bind(i64::from(limit + 1))
            .fetch_all(&self.pool)
            .await?
        };

        let has_next = rows.len() > limit as usize;
        if has_next {
            rows.truncate(limit as usize);
        }

        let items = rows
            .into_iter()
            .map(|row| EventLog {
                id: row.get("id"),
                kind: row.get("kind"),
                level: match row.get::<String, _>("level").as_str() {
                    "debug" => EventLevel::Debug,
                    "warn" => EventLevel::Warn,
                    "error" => EventLevel::Error,
                    _ => EventLevel::Info,
                },
                message: row.get("message"),
                created_at: row.get("created_at"),
            })
            .collect::<Vec<_>>();
        let next_cursor = if has_next {
            items.last().map(|e| encode_cursor(&e.created_at, &e.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }
}
