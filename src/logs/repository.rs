use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::{
    logs::event::{EventLevel, EventLog},
    utils::pagination::{clamp_limit, decode_cursor, encode_cursor, Page},
};

#[derive(Clone)]
pub struct EventLogRepository {
    pool: SqlitePool,
}

#[derive(Debug, Default, Clone)]
pub struct EventLogFilters {
    pub kind: Option<String>,
    pub level: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub search: Option<String>,
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
        self.list_filtered(EventLogFilters::default(), cursor, limit)
            .await
    }

    pub async fn list_filtered(
        &self,
        filters: EventLogFilters,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<EventLog>, sqlx::Error> {
        let limit = clamp_limit(limit);
        let mut builder = QueryBuilder::<Sqlite>::new(
            "select id, request_id, kind, level, account_id, route, model, status_code, latency_ms, message, metadata_json, created_at from event_logs",
        );
        let mut has_condition = false;
        append_optional_text_filter(&mut builder, &mut has_condition, "kind", filters.kind);
        append_optional_text_filter(&mut builder, &mut has_condition, "level", filters.level);
        append_optional_text_filter(
            &mut builder,
            &mut has_condition,
            "request_id",
            filters.request_id,
        );
        append_optional_text_filter(
            &mut builder,
            &mut has_condition,
            "account_id",
            filters.account_id,
        );
        append_optional_text_filter(&mut builder, &mut has_condition, "route", filters.route);
        append_optional_text_filter(&mut builder, &mut has_condition, "model", filters.model);
        if let Some(status_code) = filters.status_code {
            append_condition_prefix(&mut builder, &mut has_condition);
            builder.push("status_code = ");
            builder.push_bind(status_code);
        }
        if let Some(search) = filters.search {
            append_condition_prefix(&mut builder, &mut has_condition);
            let pattern = format!("%{search}%");
            builder.push("(message like ");
            builder.push_bind(pattern.clone());
            builder.push(" or metadata_json like ");
            builder.push_bind(pattern);
            builder.push(")");
        }
        if let Some(cursor) = cursor.and_then(|c| decode_cursor(&c)) {
            append_condition_prefix(&mut builder, &mut has_condition);
            builder.push("(created_at < ");
            builder.push_bind(cursor.0.clone());
            builder.push(" or (created_at = ");
            builder.push_bind(cursor.0);
            builder.push(" and id < ");
            builder.push_bind(cursor.1);
            builder.push("))");
        }
        builder.push(" order by created_at desc, id desc limit ");
        builder.push_bind(i64::from(limit + 1));
        let mut rows = builder.build().fetch_all(&self.pool).await?;

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

fn append_optional_text_filter(
    builder: &mut QueryBuilder<Sqlite>,
    has_condition: &mut bool,
    column: &'static str,
    value: Option<String>,
) {
    let Some(value) = value else {
        return;
    };
    append_condition_prefix(builder, has_condition);
    builder.push(column);
    builder.push(" = ");
    builder.push_bind(value);
}

fn append_condition_prefix(builder: &mut QueryBuilder<Sqlite>, has_condition: &mut bool) {
    if *has_condition {
        builder.push(" and ");
    } else {
        builder.push(" where ");
        *has_condition = true;
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
