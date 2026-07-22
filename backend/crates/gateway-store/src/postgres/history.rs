//! 不含正文或会话状态的模型请求历史查询。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::engine::continuation::{NativeContinuationPort, NativeContinuationStoreError};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::{
    engine::{
        continuation::{NativeContinuationPin, NativeContinuationScope, PreviousResponseId},
        credential::ProviderAccountId,
    },
    error::SafeUpstreamValue,
    routing::ProviderKind,
};
use sqlx::PgPool;

use crate::{StoreError, StoreResult, postgres_unavailable, require_nonempty};

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

    async fn find_model_request_by_client_response_id(
        &self,
        client_response_id: &str,
        caller_client_api_key_ref: &str,
    ) -> StoreResult<Option<ModelRequestHistoryRecord>>;

    async fn resolve_native_continuation_pin(
        &self,
        client_response_id: &str,
        caller_client_api_key_ref: &str,
    ) -> StoreResult<Option<NativeContinuationPin>>;
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

    async fn find_model_request_by_client_response_id(
        &self,
        client_response_id: &str,
        caller_client_api_key_ref: &str,
    ) -> StoreResult<Option<ModelRequestHistoryRecord>> {
        require_nonempty(
            "model request history",
            "client_response_id",
            client_response_id,
        )?;
        require_nonempty(
            "model request history",
            "caller_client_api_key_ref",
            caller_client_api_key_ref,
        )?;
        fetch_history_by_client_response(&self.pool, client_response_id, caller_client_api_key_ref)
            .await
    }

    async fn resolve_native_continuation_pin(
        &self,
        client_response_id: &str,
        caller_client_api_key_ref: &str,
    ) -> StoreResult<Option<NativeContinuationPin>> {
        require_nonempty(
            "native continuation",
            "client_response_id",
            client_response_id,
        )?;
        require_nonempty(
            "native continuation",
            "caller_client_api_key_ref",
            caller_client_api_key_ref,
        )?;
        let row = sqlx::query_as::<_, (String, String, String)>(
            "select mr.provider_kind, mr.provider_account_id, mr.upstream_response_id
             from model_requests mr
             join provider_accounts account
               on account.id = mr.provider_account_id
              and account.provider_kind = mr.provider_kind
             where mr.client_response_id = $1
               and mr.client_api_key_ref = $2
               and mr.outcome = 'succeeded'
               and mr.downstream_committed_at is not null
               and mr.upstream_response_id is not null",
        )
        .bind(client_response_id)
        .bind(caller_client_api_key_ref)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("resolve native continuation pin"))?;
        row.map(|row| native_pin_from_row(client_response_id, row))
            .transpose()
    }
}

impl NativeContinuationPort for PgHistoryRepository {
    fn resolve<'a>(
        &'a self,
        client_api_key_id: &'a ClientApiKeyId,
        previous_response_id: &'a PreviousResponseId,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Option<NativeContinuationPin>, NativeContinuationStoreError>,
    > {
        Box::pin(async move {
            self.resolve_native_continuation_pin(
                previous_response_id.as_str(),
                client_api_key_id.as_str(),
            )
            .await
            .map_err(|_| NativeContinuationStoreError)
        })
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

async fn fetch_history_by_client_response(
    pool: &PgPool,
    client_response_id: &str,
    caller_client_api_key_ref: &str,
) -> StoreResult<Option<ModelRequestHistoryRecord>> {
    let row = sqlx::query_as::<_, HistoryRow>(
        "select id, client_api_key_ref, requested_model_id, provider_kind,
                provider_account_ref, upstream_model_id, outcome,
                client_response_id, upstream_response_id,
                started_at, completed_at
         from model_requests
         where client_response_id = $1 and client_api_key_ref = $2",
    )
    .bind(client_response_id)
    .bind(caller_client_api_key_ref)
    .fetch_optional(pool)
    .await
    .map_err(|_| postgres_unavailable("read model request history by client response"))?;
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

fn native_pin_from_row(
    client_response_id: &str,
    row: (String, String, String),
) -> StoreResult<NativeContinuationPin> {
    let previous_response_id = PreviousResponseId::new(client_response_id.to_owned())
        .map_err(|_| invalid_native_pin("invalid client response ID"))?;
    let provider =
        ProviderKind::new(row.0).map_err(|_| invalid_native_pin("invalid provider kind"))?;
    let upstream_response_id = SafeUpstreamValue::new(row.2)
        .map_err(|_| invalid_native_pin("invalid upstream response ID"))?;
    let account = ProviderAccountId::new(row.1)
        .map_err(|_| invalid_native_pin("invalid provider account ID"))?;
    Ok(NativeContinuationPin::new(
        previous_response_id,
        upstream_response_id,
        provider,
        account,
    )
    .with_scope(NativeContinuationScope::Persisted))
}

fn invalid_native_pin(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "native continuation",
        message: message.to_owned(),
    }
}
