//! 单行 `model_requests` 生命周期与最终 usage/cost 的 PostgreSQL owner。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use gateway_core::accounting::{CostSource as CoreCostSource, Usage as CoreUsage};
use gateway_core::engine::{
    AttemptRecord as CoreAttemptRecord, ExecutionStore, IntermediateFailure,
    ModelRequestFinalization as CoreModelRequestFinalization, ModelRequestId,
    NewModelRequest as CoreNewModelRequest, RecoveryReport as CoreRecoveryReport,
    UpstreamSendState as CoreUpstreamSendState,
};
use gateway_core::error::{StoreError as CoreStoreError, StoreErrorKind as CoreStoreErrorKind};

use crate::{
    ConflictKind, DecimalAmount, StoreError, StoreResult, postgres_unavailable, require_nonempty,
};

const ENTITY: &str = "model request";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamSendState {
    NotSent,
    Sent,
    Ambiguous,
}

impl UpstreamSendState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::Sent => "sent",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRequestOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Incomplete,
}

impl ModelRequestOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostSource {
    ProviderReported,
    Calculated,
    Unavailable,
}

impl CostSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::Calculated => "calculated",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewModelRequest {
    pub id: String,
    pub client_api_key_id: Option<String>,
    pub client_api_key_ref: String,
    pub config_revision: u64,
    pub protocol: String,
    pub operation: String,
    pub endpoint: String,
    pub client_transport: String,
    pub requested_model_id: String,
    pub input_token_estimate: u64,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub compact: bool,
    pub started_at: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
}

impl NewModelRequest {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "client_api_key_ref", &self.client_api_key_ref)?;
        require_nonempty(ENTITY, "protocol", &self.protocol)?;
        require_nonempty(ENTITY, "operation", &self.operation)?;
        require_nonempty(ENTITY, "endpoint", &self.endpoint)?;
        require_nonempty(ENTITY, "client_transport", &self.client_transport)?;
        require_nonempty(ENTITY, "requested_model_id", &self.requested_model_id)?;
        if self.config_revision == 0 || self.started_at > self.deadline_at {
            return Err(invalid(
                "revision and deadline violate the frozen constraints",
            ));
        }
        if self
            .client_api_key_id
            .as_ref()
            .is_some_and(|id| id != &self.client_api_key_ref)
        {
            return Err(invalid("live client key ID must equal its historical ref"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequestAttemptStart {
    pub model_request_id: String,
    pub attempt_count: u32,
    pub provider_instance_id: String,
    pub provider_kind: String,
    pub provider_account_id: Option<String>,
    pub provider_account_ref: Option<String>,
    pub upstream_model_id: String,
    pub upstream_transport: String,
    pub http_version: Option<String>,
}

impl ModelRequestAttemptStart {
    pub fn validate(&self) -> StoreResult<()> {
        for (field, value) in [
            ("model_request_id", self.model_request_id.as_str()),
            ("provider_instance_id", self.provider_instance_id.as_str()),
            ("provider_kind", self.provider_kind.as_str()),
            ("upstream_model_id", self.upstream_model_id.as_str()),
            ("upstream_transport", self.upstream_transport.as_str()),
        ] {
            require_nonempty(ENTITY, field, value)?;
        }
        if self.attempt_count == 0 {
            return Err(invalid("attempt_count must be positive"));
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRequestUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRequestTimings {
    pub transport_decision_wait_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub headers_ms: Option<u64>,
    pub first_event_ms: Option<u64>,
    pub first_reasoning_ms: Option<u64>,
    pub first_text_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub provider_processing_ms: Option<u64>,
    pub latency_ms: Option<u64>,
}

impl ModelRequestTimings {
    fn validate(&self) -> StoreResult<()> {
        if let Some(total) = self.latency_ms {
            let phases = [
                self.transport_decision_wait_ms,
                self.connect_ms,
                self.headers_ms,
                self.first_event_ms,
                self.first_reasoning_ms,
                self.first_text_ms,
                self.first_token_ms,
                self.provider_processing_ms,
            ];
            if phases.into_iter().flatten().any(|phase| phase > total) {
                return Err(invalid("timing phase exceeds total latency"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequestFinalization {
    pub model_request_id: String,
    pub outcome: ModelRequestOutcome,
    pub upstream_send_state: UpstreamSendState,
    pub attempt_count: u32,
    pub downstream_committed_at: Option<DateTime<Utc>>,
    pub client_status_code: Option<u16>,
    pub upstream_status_code: Option<u16>,
    pub client_response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub http_version: Option<String>,
    pub error_kind: Option<String>,
    pub provider_error_code: Option<String>,
    pub error_message: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub usage: ModelRequestUsage,
    pub cost_source: CostSource,
    pub cost_amount: Option<DecimalAmount>,
    pub cost_currency: Option<String>,
    pub timings: ModelRequestTimings,
    pub completed_at: DateTime<Utc>,
}

impl ModelRequestFinalization {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "model_request_id", &self.model_request_id)?;
        for status in [self.client_status_code, self.upstream_status_code]
            .into_iter()
            .flatten()
        {
            if !(100..=599).contains(&status) {
                return Err(invalid("HTTP status must be between 100 and 599"));
            }
        }
        let cost_is_absent = self.cost_amount.is_none() && self.cost_currency.is_none();
        let cost_is_complete = self.cost_amount.is_some() && self.cost_currency.is_some();
        if (self.cost_source == CostSource::Unavailable && !cost_is_absent)
            || (self.cost_source != CostSource::Unavailable && !cost_is_complete)
        {
            return Err(invalid("cost source, amount, and currency do not agree"));
        }
        if let Some(currency) = &self.cost_currency
            && (currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()))
        {
            return Err(invalid("cost currency must be three uppercase characters"));
        }
        for (field, value) in [
            ("upstream_transport", self.upstream_transport.as_deref()),
            ("http_version", self.http_version.as_deref()),
        ] {
            if let Some(value) = value {
                require_nonempty(ENTITY, field, value)?;
            }
        }
        self.timings.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelRequestRecoveryReport {
    pub requests: u64,
}

#[async_trait]
pub trait ModelRequestRepository: Send + Sync {
    async fn insert_model_request(&self, request: NewModelRequest) -> StoreResult<()>;
    async fn begin_model_request_attempt(
        &self,
        attempt: ModelRequestAttemptStart,
    ) -> StoreResult<u32>;
    async fn mark_upstream_send_state(
        &self,
        model_request_id: &str,
        state: UpstreamSendState,
    ) -> StoreResult<bool>;
    async fn mark_downstream_committed(
        &self,
        model_request_id: &str,
        committed_at: DateTime<Utc>,
        client_status_code: Option<u16>,
    ) -> StoreResult<bool>;
    async fn record_client_status_code(
        &self,
        model_request_id: &str,
        client_status_code: u16,
    ) -> StoreResult<bool>;
    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> StoreResult<bool>;
    async fn recover_expired_model_requests(
        &self,
        now: DateTime<Utc>,
    ) -> StoreResult<ModelRequestRecoveryReport>;
}

#[derive(Clone)]
pub struct PgExecutionStore {
    pool: PgPool,
}

impl PgExecutionStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl ModelRequestRepository for PgExecutionStore {
    async fn insert_model_request(&self, request: NewModelRequest) -> StoreResult<()> {
        request.validate()?;
        sqlx::query(
            "insert into model_requests (
               id, client_api_key_id, client_api_key_ref, config_revision, protocol,
               operation, endpoint, client_transport, requested_model_id,
               input_token_estimate, client_ip, user_agent, reasoning_effort,
               reasoning_preset, request_kind, subagent_kind, compact, started_at,
               deadline_at
             ) values (
               $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::inet, $12, $13,
               $14, $15, $16, $17, $18, $19
             )",
        )
        .bind(request.id)
        .bind(request.client_api_key_id)
        .bind(request.client_api_key_ref)
        .bind(to_i64(request.config_revision, "config_revision")?)
        .bind(request.protocol)
        .bind(request.operation)
        .bind(request.endpoint)
        .bind(request.client_transport)
        .bind(request.requested_model_id)
        .bind(to_i64(
            request.input_token_estimate,
            "input_token_estimate",
        )?)
        .bind(request.client_ip)
        .bind(request.user_agent)
        .bind(request.reasoning_effort)
        .bind(request.reasoning_preset)
        .bind(request.request_kind)
        .bind(request.subagent_kind)
        .bind(request.compact)
        .bind(request.started_at)
        .bind(request.deadline_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("insert model request"))?;
        Ok(())
    }

    async fn begin_model_request_attempt(
        &self,
        attempt: ModelRequestAttemptStart,
    ) -> StoreResult<u32> {
        attempt.validate()?;
        let count = sqlx::query_scalar::<_, i32>(
            "update model_requests
             set provider_instance_id = $2,
                 provider_kind = $3,
                 provider_account_id = $4,
                 provider_account_ref = $5,
                 upstream_model_id = $6,
                 upstream_transport = $7,
                 http_version = $8,
                 attempt_count = $9,
                 upstream_send_state = 'not_sent'
             where id = $1 and outcome = 'running' and downstream_committed_at is null
               and $9 = attempt_count + 1
             returning attempt_count",
        )
        .bind(&attempt.model_request_id)
        .bind(attempt.provider_instance_id)
        .bind(attempt.provider_kind)
        .bind(attempt.provider_account_id)
        .bind(attempt.provider_account_ref)
        .bind(attempt.upstream_model_id)
        .bind(attempt.upstream_transport)
        .bind(attempt.http_version)
        .bind(
            i32::try_from(attempt.attempt_count)
                .map_err(|_| invalid("attempt_count is too large"))?,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("begin model request attempt"))?
        .ok_or(StoreError::Conflict {
            entity: ENTITY,
            id: attempt.model_request_id,
            kind: ConflictKind::DownstreamAlreadyCommitted,
        })?;
        u32::try_from(count).map_err(|_| invalid("attempt_count is invalid"))
    }

    async fn mark_upstream_send_state(
        &self,
        model_request_id: &str,
        state: UpstreamSendState,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", model_request_id)?;
        let result = sqlx::query(
            "update model_requests set upstream_send_state = $2
             where id = $1 and outcome = 'running' and attempt_count > 0",
        )
        .bind(model_request_id)
        .bind(state.as_str())
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("mark upstream send state"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn mark_downstream_committed(
        &self,
        model_request_id: &str,
        committed_at: DateTime<Utc>,
        client_status_code: Option<u16>,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", model_request_id)?;
        validate_status_code(client_status_code)?;
        let result = sqlx::query(
            "update model_requests
             set downstream_committed_at = $2, client_status_code = $3
             where id = $1 and outcome = 'running' and downstream_committed_at is null
               and client_status_code is null",
        )
        .bind(model_request_id)
        .bind(committed_at)
        .bind(client_status_code.map(i32::from))
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("mark downstream committed"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn record_client_status_code(
        &self,
        model_request_id: &str,
        client_status_code: u16,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", model_request_id)?;
        validate_status_code(Some(client_status_code))?;
        let result = sqlx::query(
            "update model_requests set client_status_code = $2
             where id = $1 and client_status_code is null",
        )
        .bind(model_request_id)
        .bind(i32::from(client_status_code))
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("record client status code"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> StoreResult<bool> {
        finalization.validate()?;
        let result = sqlx::query(
            "update model_requests
             set outcome = $2, upstream_send_state = $3, attempt_count = $4,
                 downstream_committed_at = $5,
                 client_status_code = coalesce(client_status_code, $6),
                 upstream_status_code = $7,
                 client_response_id = $8, upstream_request_id = $9, upstream_response_id = $10,
                 error_kind = $11, provider_error_code = $12, error_message = $13,
                 retry_after_ms = $14, input_tokens = $15, output_tokens = $16,
                 cached_tokens = $17, cache_write_tokens = $18, reasoning_tokens = $19,
                 total_tokens = $20, cost_source = $21, cost_amount = $22::numeric,
                 cost_currency = $23, transport_decision_wait_ms = $24, connect_ms = $25,
                 headers_ms = $26, first_event_ms = $27, first_reasoning_ms = $28,
                 first_text_ms = $29, first_token_ms = $30, provider_processing_ms = $31,
                 latency_ms = $32, completed_at = $33,
                 upstream_transport = coalesce($34, upstream_transport),
                 http_version = coalesce($35, http_version)
             where id = $1 and outcome = 'running'",
        )
        .bind(&finalization.model_request_id)
        .bind(finalization.outcome.as_str())
        .bind(finalization.upstream_send_state.as_str())
        .bind(
            i32::try_from(finalization.attempt_count)
                .map_err(|_| invalid("attempt_count is too large"))?,
        )
        .bind(finalization.downstream_committed_at)
        .bind(finalization.client_status_code.map(i32::from))
        .bind(finalization.upstream_status_code.map(i32::from))
        .bind(finalization.client_response_id)
        .bind(finalization.upstream_request_id)
        .bind(finalization.upstream_response_id)
        .bind(finalization.error_kind)
        .bind(finalization.provider_error_code)
        .bind(finalization.error_message)
        .bind(optional_i64(finalization.retry_after_ms, "retry_after_ms")?)
        .bind(optional_i64(
            finalization.usage.input_tokens,
            "input_tokens",
        )?)
        .bind(optional_i64(
            finalization.usage.output_tokens,
            "output_tokens",
        )?)
        .bind(optional_i64(
            finalization.usage.cached_tokens,
            "cached_tokens",
        )?)
        .bind(optional_i64(
            finalization.usage.cache_write_tokens,
            "cache_write_tokens",
        )?)
        .bind(optional_i64(
            finalization.usage.reasoning_tokens,
            "reasoning_tokens",
        )?)
        .bind(optional_i64(
            finalization.usage.total_tokens,
            "total_tokens",
        )?)
        .bind(finalization.cost_source.as_str())
        .bind(finalization.cost_amount.map(|amount| amount.to_string()))
        .bind(finalization.cost_currency)
        .bind(optional_i64(
            finalization.timings.transport_decision_wait_ms,
            "transport_decision_wait_ms",
        )?)
        .bind(optional_i64(finalization.timings.connect_ms, "connect_ms")?)
        .bind(optional_i64(finalization.timings.headers_ms, "headers_ms")?)
        .bind(optional_i64(
            finalization.timings.first_event_ms,
            "first_event_ms",
        )?)
        .bind(optional_i64(
            finalization.timings.first_reasoning_ms,
            "first_reasoning_ms",
        )?)
        .bind(optional_i64(
            finalization.timings.first_text_ms,
            "first_text_ms",
        )?)
        .bind(optional_i64(
            finalization.timings.first_token_ms,
            "first_token_ms",
        )?)
        .bind(optional_i64(
            finalization.timings.provider_processing_ms,
            "provider_processing_ms",
        )?)
        .bind(optional_i64(finalization.timings.latency_ms, "latency_ms")?)
        .bind(finalization.completed_at)
        .bind(finalization.upstream_transport)
        .bind(finalization.http_version)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("finalize model request"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn recover_expired_model_requests(
        &self,
        now: DateTime<Utc>,
    ) -> StoreResult<ModelRequestRecoveryReport> {
        let result = sqlx::query(
            "update model_requests
             set outcome = 'incomplete', error_kind = 'process_interrupted',
                 error_message = 'request did not reach a terminal state', completed_at = $1
             where outcome = 'running' and deadline_at <= $1",
        )
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("recover expired model requests"))?;
        Ok(ModelRequestRecoveryReport {
            requests: result.rows_affected(),
        })
    }
}

#[async_trait]
impl ExecutionStore for PgExecutionStore {
    async fn create_model_request(
        &self,
        request: CoreNewModelRequest,
    ) -> Result<(), CoreStoreError> {
        self.insert_model_request(NewModelRequest {
            id: request.id.as_str().to_owned(),
            client_api_key_id: request
                .client_api_key_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            client_api_key_ref: request.client_api_key_ref.as_str().to_owned(),
            config_revision: request.config_revision.get(),
            protocol: request.protocol,
            operation: request.operation.as_str().to_owned(),
            endpoint: request.endpoint,
            client_transport: request.client_transport,
            requested_model_id: request.requested_model.as_str().to_owned(),
            input_token_estimate: request.input_token_estimate,
            client_ip: request.client_ip.map(|address| address.to_string()),
            user_agent: request.user_agent,
            reasoning_effort: request.reasoning_effort,
            reasoning_preset: request.reasoning_preset,
            request_kind: request.request_kind,
            subagent_kind: request.subagent_kind,
            compact: request.compact,
            started_at: DateTime::<Utc>::from(request.started_at),
            deadline_at: DateTime::<Utc>::from(request.deadline_at),
        })
        .await
        .map_err(core_store_error)
    }

    async fn record_attempt(&self, attempt: CoreAttemptRecord) -> Result<(), CoreStoreError> {
        let expected_count = attempt.attempt_count.get();
        let persisted = self
            .begin_model_request_attempt(ModelRequestAttemptStart {
                model_request_id: attempt.request_id.as_str().to_owned(),
                attempt_count: expected_count,
                provider_instance_id: attempt.provider_instance_id.as_str().to_owned(),
                provider_kind: attempt.provider_kind.as_str().to_owned(),
                provider_account_id: attempt
                    .provider_account_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
                provider_account_ref: attempt
                    .provider_account_ref
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
                upstream_model_id: attempt.upstream_model_id.as_str().to_owned(),
                upstream_transport: attempt.upstream_transport,
                http_version: attempt.http_version,
            })
            .await
            .map_err(core_store_error)?;
        if persisted == expected_count {
            Ok(())
        } else {
            Err(CoreStoreError::new(CoreStoreErrorKind::InvalidState))
        }
    }

    async fn mark_send_state(
        &self,
        request_id: &ModelRequestId,
        state: CoreUpstreamSendState,
    ) -> Result<(), CoreStoreError> {
        let updated = self
            .mark_upstream_send_state(request_id.as_str(), send_state_from_core(state))
            .await
            .map_err(core_store_error)?;
        require_core_update(updated)
    }

    async fn mark_downstream_committed(
        &self,
        request_id: &ModelRequestId,
        committed_at: std::time::SystemTime,
        client_status_code: Option<u16>,
    ) -> Result<(), CoreStoreError> {
        let updated = ModelRequestRepository::mark_downstream_committed(
            self,
            request_id.as_str(),
            DateTime::<Utc>::from(committed_at),
            client_status_code,
        )
        .await
        .map_err(core_store_error)?;
        require_core_update(updated)
    }

    async fn record_client_status(
        &self,
        request_id: &ModelRequestId,
        client_status_code: u16,
    ) -> Result<(), CoreStoreError> {
        let updated = ModelRequestRepository::record_client_status_code(
            self,
            request_id.as_str(),
            client_status_code,
        )
        .await
        .map_err(core_store_error)?;
        require_core_update(updated)
    }

    async fn record_intermediate_failure(
        &self,
        failure: IntermediateFailure,
    ) -> Result<(), CoreStoreError> {
        let error = failure.error;
        let retry_after_ms = error
            .retry_after()
            .map(|duration| u64::try_from(duration.as_millis()))
            .transpose()
            .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
        super::OpsEventRepository::append_ops_event(
            &super::PgOpsEventRepository::new(self.pool.clone()),
            super::OpsEvent {
                id: Uuid::now_v7().to_string(),
                model_request_id: Some(failure.request_id.as_str().to_owned()),
                attempt_index: Some(failure.attempt_index.get()),
                level: super::OpsEventLevel::Warning,
                component: "routing".to_owned(),
                operation: failure.trigger.as_str().to_owned(),
                provider_instance_id: Some(failure.instance_id.as_str().to_owned()),
                provider_kind: Some(failure.provider_kind.as_str().to_owned()),
                provider_account_id: failure.account_id.as_ref().map(|id| id.as_str().to_owned()),
                provider_account_ref: failure.account_id.as_ref().map(|id| id.as_str().to_owned()),
                upstream_model_id: Some(failure.upstream_model_id.as_str().to_owned()),
                failure_kind: error.kind().as_str().to_owned(),
                status_code: error.upstream_status().or(failure.upstream_status_code),
                provider_error_code: error.upstream_code().map(|code| code.as_str().to_owned()),
                retry_after_ms,
                upstream_request_id: error
                    .upstream_request_id()
                    .map(|id| id.as_str().to_owned())
                    .or(failure.upstream_request_id),
                latency_ms: Some(
                    u64::try_from(failure.latency.as_millis())
                        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
                ),
                message: "intermediate upstream failure".to_owned(),
                occurrence_count: 1,
                created_at: Utc::now(),
            },
        )
        .await
        .map_err(core_store_error)
    }

    async fn finalize_model_request(
        &self,
        finalization: CoreModelRequestFinalization,
    ) -> Result<(), CoreStoreError> {
        let (cost_source, cost_amount, cost_currency) = match finalization.cost.total() {
            Some(total) => (
                cost_source_from_core(finalization.cost.source()),
                Some(
                    total
                        .amount()
                        .to_string()
                        .parse()
                        .map_err(core_store_error)?,
                ),
                Some(total.currency().as_str().to_owned()),
            ),
            None => (CostSource::Unavailable, None, None),
        };
        let error_kind = finalization
            .error
            .as_ref()
            .map(|error| error.kind().as_str().to_owned());
        let error_message = finalization
            .error
            .as_ref()
            .map(|error| error.safe_message().to_owned());
        let completed = ModelRequestRepository::finalize_model_request(
            self,
            ModelRequestFinalization {
                model_request_id: finalization.request_id.as_str().to_owned(),
                outcome: outcome_from_core(finalization.outcome)?,
                upstream_send_state: send_state_from_core(finalization.send_state),
                attempt_count: finalization.attempt_count,
                downstream_committed_at: finalization
                    .downstream_committed_at
                    .map(DateTime::<Utc>::from),
                client_status_code: finalization.client_status_code,
                upstream_status_code: finalization.upstream_status_code,
                client_response_id: finalization.client_response_id,
                upstream_request_id: finalization.upstream_request_id,
                upstream_response_id: finalization.upstream_response_id,
                upstream_transport: finalization.upstream_transport,
                http_version: finalization.http_version,
                error_kind,
                provider_error_code: finalization.provider_error_code,
                error_message,
                retry_after_ms: finalization.retry_after_ms,
                usage: usage_from_core(finalization.usage),
                cost_source,
                cost_amount,
                cost_currency,
                timings: ModelRequestTimings {
                    transport_decision_wait_ms: finalization.timings.transport_decision_wait_ms,
                    connect_ms: finalization.timings.connect_ms,
                    headers_ms: finalization.timings.headers_ms,
                    first_event_ms: finalization.timings.first_event_ms,
                    first_reasoning_ms: finalization.timings.first_reasoning_ms,
                    first_text_ms: finalization.timings.first_text_ms,
                    first_token_ms: finalization.timings.first_token_ms,
                    provider_processing_ms: finalization.timings.provider_processing_ms,
                    latency_ms: finalization.timings.latency_ms,
                },
                completed_at: DateTime::<Utc>::from(finalization.completed_at),
            },
        )
        .await
        .map_err(core_store_error)?;
        require_core_update(completed)
    }

    async fn recover_expired(
        &self,
        now: std::time::SystemTime,
    ) -> Result<CoreRecoveryReport, CoreStoreError> {
        let report = self
            .recover_expired_model_requests(DateTime::<Utc>::from(now))
            .await
            .map_err(core_store_error)?;
        Ok(CoreRecoveryReport {
            requests: report.requests,
        })
    }
}

fn usage_from_core(usage: CoreUsage) -> ModelRequestUsage {
    ModelRequestUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        total_tokens: usage.total_tokens,
    }
}

const fn send_state_from_core(value: CoreUpstreamSendState) -> UpstreamSendState {
    match value {
        CoreUpstreamSendState::NotSent => UpstreamSendState::NotSent,
        CoreUpstreamSendState::Sent => UpstreamSendState::Sent,
        CoreUpstreamSendState::Ambiguous => UpstreamSendState::Ambiguous,
    }
}

fn outcome_from_core(
    value: gateway_core::engine::ExecutionOutcome,
) -> Result<ModelRequestOutcome, CoreStoreError> {
    match value {
        gateway_core::engine::ExecutionOutcome::Running => {
            Err(CoreStoreError::new(CoreStoreErrorKind::InvalidState))
        }
        gateway_core::engine::ExecutionOutcome::Succeeded => Ok(ModelRequestOutcome::Succeeded),
        gateway_core::engine::ExecutionOutcome::Failed => Ok(ModelRequestOutcome::Failed),
        gateway_core::engine::ExecutionOutcome::Cancelled => Ok(ModelRequestOutcome::Cancelled),
        gateway_core::engine::ExecutionOutcome::Incomplete => Ok(ModelRequestOutcome::Incomplete),
    }
}

const fn cost_source_from_core(value: CoreCostSource) -> CostSource {
    match value {
        CoreCostSource::ProviderReported => CostSource::ProviderReported,
        CoreCostSource::Calculated => CostSource::Calculated,
        CoreCostSource::Unavailable => CostSource::Unavailable,
    }
}

fn require_core_update(updated: bool) -> Result<(), CoreStoreError> {
    if updated {
        Ok(())
    } else {
        Err(CoreStoreError::new(CoreStoreErrorKind::InvalidState))
    }
}

fn core_store_error(error: StoreError) -> CoreStoreError {
    let kind = match error {
        StoreError::Unavailable { .. } => CoreStoreErrorKind::Unavailable,
        StoreError::Conflict { .. } => CoreStoreErrorKind::Conflict,
        StoreError::NotFound { .. } | StoreError::InvalidData { .. } => {
            CoreStoreErrorKind::InvalidData
        }
    };
    CoreStoreError::new(kind)
}

fn optional_i64(value: Option<u64>, field: &'static str) -> StoreResult<Option<i64>> {
    value.map(|value| to_i64(value, field)).transpose()
}

fn validate_status_code(status: Option<u16>) -> StoreResult<()> {
    if status.is_some_and(|status| !(100..=599).contains(&status)) {
        return Err(invalid("HTTP status must be between 100 and 599"));
    }
    Ok(())
}

fn to_i64(value: u64, field: &'static str) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| invalid(field))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: message.to_owned(),
    }
}
