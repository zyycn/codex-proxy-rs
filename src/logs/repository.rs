use sqlx::{Row, SqlitePool};

use crate::{
    logs::event::{EventLevel, EventLog},
    utils::pagination::{clamp_limit, decode_cursor, encode_cursor, Page},
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
        let metadata_json = serde_json::to_string(&event.metadata).unwrap_or_else(|_| "{}".into());
        sqlx::query(
            "insert into event_logs (id, request_id, kind, level, account_id, route, model, status_code, latency_ms, message, metadata_json, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.id)
        .bind(event.request_id)
        .bind(event.kind)
        .bind(event.level.as_str())
        .bind(event.account_id)
        .bind(event.route)
        .bind(event.model)
        .bind(event.status_code)
        .bind(event.latency_ms)
        .bind(event.message)
        .bind(metadata_json)
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
                "select id, request_id, kind, level, account_id, route, model, status_code, latency_ms, message, metadata_json, created_at from event_logs where (created_at < ? or (created_at = ? and id < ?)) order by created_at desc, id desc limit ?",
            )
            .bind(&cursor.0)
            .bind(&cursor.0)
            .bind(&cursor.1)
            .bind(i64::from(limit + 1))
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select id, request_id, kind, level, account_id, route, model, status_code, latency_ms, message, metadata_json, created_at from event_logs order by created_at desc, id desc limit ?",
            )
            .bind(i64::from(limit + 1))
            .fetch_all(&self.pool)
            .await?
        };

        let has_next = rows.len() > limit as usize;
        if has_next {
            rows.truncate(limit as usize);
        }

        let items = rows.into_iter().map(row_to_event_log).collect::<Vec<_>>();
        let next_cursor = if has_next {
            items.last().map(|e| encode_cursor(&e.created_at, &e.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    pub async fn get(&self, id: &str) -> Result<Option<EventLog>, sqlx::Error> {
        sqlx::query(
            "select id, request_id, kind, level, account_id, route, model, status_code, latency_ms, message, metadata_json, created_at from event_logs where id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(row_to_event_log))
    }

    pub async fn count(&self) -> Result<u64, sqlx::Error> {
        let row = sqlx::query("select count(*) as count from event_logs")
            .fetch_one(&self.pool)
            .await?;
        let count = row.get::<i64, _>("count");
        Ok(count.max(0) as u64)
    }

    pub async fn clear(&self) -> Result<u64, sqlx::Error> {
        sqlx::query("delete from event_logs")
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
    }
}

fn row_to_event_log(row: sqlx::sqlite::SqliteRow) -> EventLog {
    EventLog {
        id: row.get("id"),
        request_id: row.get("request_id"),
        kind: row.get("kind"),
        level: match row.get::<String, _>("level").as_str() {
            "debug" => EventLevel::Debug,
            "warn" => EventLevel::Warn,
            "error" => EventLevel::Error,
            _ => EventLevel::Info,
        },
        account_id: row.get("account_id"),
        route: row.get("route"),
        model: row.get("model"),
        status_code: row.get("status_code"),
        latency_ms: row.get("latency_ms"),
        message: row.get("message"),
        metadata: serde_json::from_str(&row.get::<String, _>("metadata_json"))
            .unwrap_or_else(|_| serde_json::json!({})),
        created_at: row.get("created_at"),
    }
}
