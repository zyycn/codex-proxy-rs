//! `ops_events` 中间失败与后台故障事实的 PostgreSQL owner。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{StoreError, StoreResult, postgres_unavailable, require_nonempty};

const ENTITY: &str = "ops event";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpsEventLevel {
    Warning,
    Error,
}

impl OpsEventLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsEvent {
    pub id: String,
    pub model_request_id: Option<String>,
    pub attempt_index: Option<u32>,
    pub level: OpsEventLevel,
    pub component: String,
    pub operation: String,
    pub provider_instance_id: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_account_id: Option<String>,
    pub provider_account_ref: Option<String>,
    pub upstream_model_id: Option<String>,
    pub failure_kind: String,
    pub status_code: Option<u16>,
    pub provider_error_code: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub upstream_request_id: Option<String>,
    pub latency_ms: Option<u64>,
    pub message: String,
    pub occurrence_count: u32,
    pub created_at: DateTime<Utc>,
}

impl OpsEvent {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "component", &self.component)?;
        require_nonempty(ENTITY, "operation", &self.operation)?;
        require_nonempty(ENTITY, "failure_kind", &self.failure_kind)?;
        require_nonempty(ENTITY, "message", &self.message)?;
        let request_scoped = self.model_request_id.is_some();
        let has_attempt = self.attempt_index.is_some_and(|index| index > 0);
        if request_scoped != has_attempt {
            return Err(invalid(
                "request-scoped events require a model request and a positive attempt index",
            ));
        }
        if self.attempt_index == Some(0) || self.occurrence_count == 0 {
            return Err(invalid(
                "attempt_index and occurrence_count must be positive",
            ));
        }
        if self
            .status_code
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(invalid("status_code must be between 100 and 599"));
        }
        if self
            .provider_account_id
            .as_ref()
            .is_some_and(|id| Some(id) != self.provider_account_ref.as_ref())
        {
            return Err(invalid(
                "live provider account ID must equal its historical ref",
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait OpsEventRepository: Send + Sync {
    async fn append_ops_event(&self, event: OpsEvent) -> StoreResult<()>;
}

#[derive(Clone)]
pub struct PgOpsEventRepository {
    pool: PgPool,
}

impl PgOpsEventRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OpsEventRepository for PgOpsEventRepository {
    async fn append_ops_event(&self, event: OpsEvent) -> StoreResult<()> {
        event.validate()?;
        sqlx::query(
            "insert into ops_events (
               id, model_request_id, attempt_index, level, component, operation,
               provider_instance_id, provider_kind,
               provider_account_id, provider_account_ref, upstream_model_id,
               failure_kind, status_code, provider_error_code, retry_after_ms,
               upstream_request_id, latency_ms, message, occurrence_count, created_at
             ) values (
               $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
               $14, $15, $16, $17, $18, $19, $20
             )",
        )
        .bind(event.id)
        .bind(event.model_request_id)
        .bind(
            event
                .attempt_index
                .map(i32::try_from)
                .transpose()
                .map_err(|_| invalid("attempt index is too large"))?,
        )
        .bind(event.level.as_str())
        .bind(event.component)
        .bind(event.operation)
        .bind(event.provider_instance_id)
        .bind(event.provider_kind)
        .bind(event.provider_account_id)
        .bind(event.provider_account_ref)
        .bind(event.upstream_model_id)
        .bind(event.failure_kind)
        .bind(event.status_code.map(i32::from))
        .bind(event.provider_error_code)
        .bind(
            event
                .retry_after_ms
                .map(i64::try_from)
                .transpose()
                .map_err(|_| invalid("retry_after_ms is too large"))?,
        )
        .bind(event.upstream_request_id)
        .bind(
            event
                .latency_ms
                .map(i64::try_from)
                .transpose()
                .map_err(|_| invalid("latency_ms is too large"))?,
        )
        .bind(event.message)
        .bind(
            i32::try_from(event.occurrence_count)
                .map_err(|_| invalid("occurrence_count is too large"))?,
        )
        .bind(event.created_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("append ops event"))?;
        Ok(())
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: message.to_owned(),
    }
}
