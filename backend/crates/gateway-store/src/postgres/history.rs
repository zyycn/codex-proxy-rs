//! 不含正文或会话状态的模型请求历史查询。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{StoreResult, postgres_unavailable, require_nonempty};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequestHistoryRecord {
    pub id: String,
    pub client_api_key_ref: String,
    pub requested_model_id: String,
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
    pub upstream_model_id: Option<String>,
    pub outcome: String,
    pub client_response_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait ModelRequestHistoryRepository: Send + Sync {
    async fn find_model_request_history(
        &self,
        model_request_id: &str,
    ) -> StoreResult<Option<ModelRequestHistoryRecord>>;
}

#[derive(Clone)]
pub struct PgHistoryRepository {
    pool: PgPool,
}

impl PgHistoryRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ModelRequestHistoryRepository for PgHistoryRepository {
    async fn find_model_request_history(
        &self,
        model_request_id: &str,
    ) -> StoreResult<Option<ModelRequestHistoryRecord>> {
        require_nonempty(
            "model request history",
            "model_request_id",
            model_request_id,
        )?;
        fetch_history(&self.pool, "id", model_request_id).await
    }
}

async fn fetch_history(
    pool: &PgPool,
    lookup: &'static str,
    value: &str,
) -> StoreResult<Option<ModelRequestHistoryRecord>> {
    let row = match lookup {
        "id" => {
            sqlx::query_as::<_, HistoryRow>(
                "select id, client_api_key_ref, requested_model_id, provider_kind,
                    provider_account_ref, upstream_model_id, outcome,
                    client_response_id, upstream_response_id,
                    started_at, completed_at
             from model_requests where id = $1",
            )
            .bind(value)
            .fetch_optional(pool)
            .await
        }
        _ => return Ok(None),
    }
    .map_err(|_| postgres_unavailable("read model request history"))?;
    Ok(row.map(history_from_row))
}

type HistoryRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

fn history_from_row(row: HistoryRow) -> ModelRequestHistoryRecord {
    ModelRequestHistoryRecord {
        id: row.0,
        client_api_key_ref: row.1,
        requested_model_id: row.2,
        provider_kind: row.3,
        provider_account_ref: row.4,
        upstream_model_id: row.5,
        outcome: row.6,
        client_response_id: row.7,
        upstream_response_id: row.8,
        started_at: row.9,
        completed_at: row.10,
    }
}
