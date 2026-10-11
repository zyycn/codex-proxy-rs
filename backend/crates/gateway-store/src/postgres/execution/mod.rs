//! 单行 `model_requests` 生命周期与最终 usage/cost 的 PostgreSQL owner

use crate::core_store_error;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use gateway_core::engine::{
    AttemptRecord as CoreAttemptRecord, ExecutionStore, IntermediateFailure,
    ModelRequestFinalization as CoreModelRequestFinalization, ModelRequestId,
    NewModelRequest as CoreNewModelRequest, ProbeFailure, RecoveryReport as CoreRecoveryReport,
};
use gateway_core::error::{
    ProviderErrorKind, StoreError as CoreStoreError, StoreErrorKind as CoreStoreErrorKind,
};
use gateway_core::metering::CostSource as CoreCostSource;
use gateway_core::routing::{AccountRoutingScopeKind, AccountRoutingSnapshot};
use gateway_core::upstream::UpstreamSendState as CoreUpstreamSendState;

use crate::{
    ConflictKind, DecimalAmount, StoreError, StoreResult, postgres_unavailable, require_nonempty,
};

mod request_observation;

use request_observation::RequestObservation;

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
    pub client_api_key_name_snapshot: Option<String>,
    pub client_api_key_ref: String,
    pub config_revision: u64,
    pub routing_scope: String,
    pub routing_group_refs: Vec<String>,
    pub routing_group_names_snapshot: Value,
    pub protocol: String,
    pub operation: String,
    pub endpoint: String,
    pub client_transport: String,
    pub requested_model_id: Option<String>,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub compact: bool,
    pub continuation: ContinuationRequestObservation,
    pub image_generation_requested: bool,
    pub admission_decision_ms: Option<u64>,
    pub started_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContinuationRequestObservation {
    pub affinity_hash: Option<String>,
    pub previous_response_id_hash: Option<String>,
    pub requested: bool,
}

impl NewModelRequest {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "client_api_key_ref", &self.client_api_key_ref)?;
        require_nonempty(ENTITY, "protocol", &self.protocol)?;
        require_nonempty(ENTITY, "operation", &self.operation)?;
        require_nonempty(ENTITY, "endpoint", &self.endpoint)?;
        require_nonempty(ENTITY, "client_transport", &self.client_transport)?;
        if let Some(requested_model_id) = self.requested_model_id.as_deref() {
            require_nonempty(ENTITY, "requested_model_id", requested_model_id)?;
        }
        validate_routing_snapshot(
            &self.routing_scope,
            &self.routing_group_refs,
            &self.routing_group_names_snapshot,
        )?;
        if self.config_revision == 0 || self.started_at > self.lease_expires_at {
            return Err(invalid(
                "revision and deadline violate the frozen constraints",
            ));
        }
        self.continuation.validate()?;
        Ok(())
    }
}

impl ContinuationRequestObservation {
    fn validate(&self) -> StoreResult<()> {
        for value in [
            self.affinity_hash.as_deref(),
            self.previous_response_id_hash.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(invalid("continuation observation hash is invalid"));
            }
        }
        if !self.requested && self.previous_response_id_hash.is_some() {
            return Err(invalid("continuation request facts do not agree"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequestAttemptStart {
    pub model_request_id: String,
    pub attempt_count: u32,
    pub provider_kind: String,
    pub account_snapshot: Option<gateway_core::account::AccountHistorySnapshot>,
    pub provider_account_ref: Option<String>,
    pub upstream_model_id: Option<String>,
    pub upstream_transport: String,
    pub http_version: Option<String>,
    pub account_selection_wait_ms: Option<u64>,
    pub capacity_used_slots: Option<u64>,
    pub capacity_total_slots: Option<u64>,
}

impl ModelRequestAttemptStart {
    pub fn validate(&self) -> StoreResult<()> {
        for (field, value) in [
            ("model_request_id", self.model_request_id.as_str()),
            ("provider_kind", self.provider_kind.as_str()),
            ("upstream_transport", self.upstream_transport.as_str()),
        ] {
            require_nonempty(ENTITY, field, value)?;
        }
        if let Some(upstream_model_id) = self.upstream_model_id.as_deref() {
            require_nonempty(ENTITY, "upstream_model_id", upstream_model_id)?;
        }
        if self.attempt_count == 0 {
            return Err(invalid("attempt_count must be positive"));
        }
        match (self.capacity_used_slots, self.capacity_total_slots) {
            (None, None) => {}
            (Some(used), Some(total)) if total > 0 && used <= total => {}
            _ => return Err(invalid("capacity snapshot is invalid")),
        }
        Ok(())
    }
}

pub use gateway_core::{engine::ModelRequestTimings, metering::Usage as ModelRequestUsage};

fn validate_timings(timings: &ModelRequestTimings) -> StoreResult<()> {
    if let Some(total) = timings.latency_ms {
        let phases = [
            timings.transport_decision_wait_ms,
            timings.connect_ms,
            timings.headers_ms,
            timings.first_event_ms,
            timings.first_reasoning_ms,
            timings.first_text_ms,
            timings.first_token_ms,
        ];
        if phases.into_iter().flatten().any(|phase| phase > total) {
            return Err(invalid("timing phase exceeds total latency"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequestFinalization {
    pub billing_snapshot_json: Option<Value>,
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
    pub websocket_pool: Option<String>,
    pub service_tier: Option<String>,
    pub upstream_response_model: Option<String>,
    pub provider_metadata_json: Option<Value>,
    pub diagnostic_trace_json: Option<Value>,
    pub error_kind: Option<String>,
    pub provider_error_code: Option<String>,
    pub error_message: Option<String>,
    pub error_details: Option<String>,
    pub continuation_unavailable_reason: Option<String>,
    pub upstream_connection_id: Option<String>,
    pub upstream_connection_exit_reason: Option<String>,
    pub upstream_connection_age_ms: Option<u64>,
    pub upstream_connection_idle_ms: Option<u64>,
    pub retry_after_ms: Option<u64>,
    pub usage: ModelRequestUsage,
    pub image_generation_succeeded: Option<bool>,
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
        if self
            .websocket_pool
            .as_deref()
            .is_some_and(|kind| !matches!(kind, "new" | "reuse"))
        {
            return Err(invalid("websocket pool must be new or reuse"));
        }
        if let Some(service_tier) = self.service_tier.as_deref() {
            require_nonempty(ENTITY, "service_tier", service_tier)?;
            if service_tier.len() > 64 || service_tier.chars().any(char::is_control) {
                return Err(invalid("service tier is invalid"));
            }
        }
        if let Some(model) = self.upstream_response_model.as_deref() {
            require_nonempty(ENTITY, "upstream_response_model", model)?;
            if model.len() > 256 || model.chars().any(char::is_control) {
                return Err(invalid("upstream response model is invalid"));
            }
        }
        if self
            .provider_metadata_json
            .as_ref()
            .is_some_and(|metadata| !metadata.is_object())
        {
            return Err(invalid("provider observation must be a JSON object"));
        }
        if self
            .diagnostic_trace_json
            .as_ref()
            .is_some_and(|trace| !trace.is_object() || trace.to_string().len() > 64 * 1024)
        {
            return Err(invalid(
                "diagnostic trace must be a JSON object within 64 KiB",
            ));
        }
        validate_optional_stable_reason(
            self.continuation_unavailable_reason.as_deref(),
            "continuation unavailable reason",
        )?;
        validate_connection_observation(self)?;
        validate_timings(&self.timings)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelRequestRecoveryReport {
    pub requests: u64,
}

#[async_trait]
pub trait ModelRequestRepository: Send + Sync {
    async fn insert_model_request(&self, request: NewModelRequest) -> StoreResult<()>;
    /// 请求行与首次 attempt 列一次插入；`attempt.attempt_count` 必须为 1
    async fn insert_model_request_with_first_attempt(
        &self,
        request: NewModelRequest,
        attempt: ModelRequestAttemptStart,
    ) -> StoreResult<()>;
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
        let observation = RequestObservation::initial(&request)?;
        sqlx::query(
            "with inserted as (
               insert into model_requests (
                 id, client_api_key_ref, operation, client_transport,
                 requested_model_id, image_generation_requested, started_at,
                 continuation_affinity_hash, continuation_requested, request_kind, request_observation_json
               ) values ($1, $2, $3, $4, $5, $6, $7, $9, $10, $11, $12)
               returning id
             ) insert into model_request_leases (request_id, lease_expires_at)
               select id, $8 from inserted",
        )
        .bind(request.id)
        .bind(request.client_api_key_ref)
        .bind(request.operation)
        .bind(request.client_transport)
        .bind(request.requested_model_id)
        .bind(request.image_generation_requested)
        .bind(request.started_at)
        .bind(request.lease_expires_at)
        .bind(request.continuation.affinity_hash)
        .bind(request.continuation.requested)
        .bind(request.request_kind)
        .bind(observation)
        .execute(&self.pool)
        .await
        .map_err(|source| postgres_unavailable("insert model request", source))?;
        Ok(())
    }

    async fn insert_model_request_with_first_attempt(
        &self,
        request: NewModelRequest,
        attempt: ModelRequestAttemptStart,
    ) -> StoreResult<()> {
        request.validate()?;
        attempt.validate()?;
        if attempt.attempt_count != 1 || attempt.model_request_id != request.id {
            return Err(invalid("first attempt must target the inserted request"));
        }
        let observation = RequestObservation::initial(&request)?;
        sqlx::query(
            "with inserted as (
               insert into model_requests (
                 id, client_api_key_ref, operation, client_transport,
                 requested_model_id, image_generation_requested, started_at,
                 continuation_affinity_hash, continuation_requested, request_kind, request_observation_json,
                 provider_kind, provider_account_ref, upstream_model_id,
                 upstream_transport, attempt_count, upstream_send_state
               ) select $1, $2, $3, $4, $5, $6, $7, $9, $10, $11,
                 $12::jsonb || jsonb_build_object(
                   'account', coalesce($14::jsonb, '{}'::jsonb),
                   'transport', jsonb_strip_nulls(jsonb_build_object('httpVersion', $18::text)),
                   'scheduling', ($12::jsonb -> 'scheduling') || jsonb_strip_nulls(jsonb_build_object(
                     'accountSelectionWaitMs', $19::bigint,
                     'capacityUsedSlots', $20::bigint, 'capacityTotalSlots', $21::bigint))),
                 $13, $15, $16, $17, 1, 'not_sent'
               returning id
             ) insert into model_request_leases (request_id, lease_expires_at)
               select id, $8 from inserted",
        )
        .bind(request.id)
        .bind(request.client_api_key_ref)
        .bind(request.operation)
        .bind(request.client_transport)
        .bind(request.requested_model_id)
        .bind(request.image_generation_requested)
        .bind(request.started_at)
        .bind(request.lease_expires_at)
        .bind(request.continuation.affinity_hash)
        .bind(request.continuation.requested)
        .bind(request.request_kind)
        .bind(observation)
        .bind(attempt.provider_kind)
        .bind(attempt.account_snapshot.map(sqlx::types::Json))
        .bind(attempt.provider_account_ref)
        .bind(attempt.upstream_model_id)
        .bind(attempt.upstream_transport)
        .bind(attempt.http_version)
        .bind(optional_i64(
            attempt.account_selection_wait_ms,
            "account_selection_wait_ms",
        )?)
        .bind(optional_i64(
            attempt.capacity_used_slots,
            "capacity_used_slots",
        )?)
        .bind(optional_i64(
            attempt.capacity_total_slots,
            "capacity_total_slots",
        )?)
        .execute(&self.pool)
        .await
        .map_err(|source| {
            postgres_unavailable("insert model request with first attempt", source)
        })?;
        Ok(())
    }

    async fn begin_model_request_attempt(
        &self,
        attempt: ModelRequestAttemptStart,
    ) -> StoreResult<u32> {
        attempt.validate()?;
        // upstream_send_state 是请求级单调水位（sent > ambiguous > not_sent），
        // 由 mark/finalize 抬升；开启新 attempt 不得把已持久化的 sent 重置回
        // not_sent——崩溃恢复的终态写回会原样继承本列
        let count = sqlx::query_scalar::<_, i32>(
            "with observed as materialized (
               select id,
                 (request_observation_json #>> '{scheduling,accountSelectionWaitMs}')::bigint as selection_wait_ms,
                 (request_observation_json #>> '{scheduling,capacityUsedSlots}')::bigint as used_slots,
                 (request_observation_json #>> '{scheduling,capacityTotalSlots}')::bigint as total_slots
               from model_requests where id = $1 for update
             ) update model_requests
             set provider_kind = $2,
                 provider_account_ref = $4,
                 upstream_model_id = $5,
                 upstream_transport = $6,
                 attempt_count = $8,
                 request_observation_json = request_observation_json || jsonb_build_object(
                   'account', coalesce($3::jsonb, '{}'::jsonb),
                   'transport', jsonb_strip_nulls(jsonb_build_object('httpVersion', $7::text)),
                   'scheduling', coalesce(request_observation_json -> 'scheduling', '{}'::jsonb) || jsonb_strip_nulls(jsonb_build_object(
                     'accountSelectionWaitMs', case when $9::bigint is null
                       then observed.selection_wait_ms
                       else coalesce(observed.selection_wait_ms, 0) + $9 end,
                     'capacityUsedSlots', coalesce($10::bigint, observed.used_slots),
                     'capacityTotalSlots', coalesce($11::bigint, observed.total_slots))))
             from observed
             where model_requests.id = observed.id and outcome = 'running' and downstream_committed_at is null
               and $8 = attempt_count + 1
             returning attempt_count",
        )
        .bind(&attempt.model_request_id)
        .bind(attempt.provider_kind)
        .bind(attempt.account_snapshot.map(sqlx::types::Json))
        .bind(attempt.provider_account_ref)
        .bind(attempt.upstream_model_id)
        .bind(attempt.upstream_transport)
        .bind(attempt.http_version)
        .bind(
            i32::try_from(attempt.attempt_count)
                .map_err(|source| invalid("attempt_count is too large").with_source(source))?,
        )
        .bind(optional_i64(
            attempt.account_selection_wait_ms,
            "account_selection_wait_ms",
        )?)
        .bind(optional_i64(
            attempt.capacity_used_slots,
            "capacity_used_slots",
        )?)
        .bind(optional_i64(
            attempt.capacity_total_slots,
            "capacity_total_slots",
        )?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| postgres_unavailable("begin model request attempt", source))?
        .ok_or(StoreError::Conflict {
            source: None,
            entity: ENTITY,
            id: attempt.model_request_id,
            kind: ConflictKind::DownstreamAlreadyCommitted,
        })?;
        u32::try_from(count)
            .map_err(|source| invalid("attempt_count is invalid").with_source(source))
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
        .map_err(|source| postgres_unavailable("mark upstream send state", source))?;
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
        .map_err(|source| postgres_unavailable("mark downstream committed", source))?;
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
        .map_err(|source| postgres_unavailable("record client status code", source))?;
        Ok(result.rows_affected() == 1)
    }

    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> StoreResult<bool> {
        finalization.validate()?;
        let observation = RequestObservation::finalized(&finalization)?;
        // 墙上时间可能回拨；终态必须可落盘，实际耗时仍保留 Core 的单调时钟观测
        let finalized = sqlx::query_scalar::<_, i64>(
            "with finalized as (
             update model_requests
             set outcome = $2, upstream_send_state = $3, attempt_count = $4,
                 downstream_committed_at = $5,
                 client_status_code = coalesce(client_status_code, $6), upstream_status_code = $7,
                 client_response_id = $8, upstream_request_id = $9, upstream_response_id = $10,
                 error_kind = $11, input_tokens = $12, output_tokens = $13,
                 cached_input_tokens = $14, cache_write_input_tokens = $15, reasoning_output_tokens = $16,
                 image_input_tokens = $17, image_output_tokens = $18, total_tokens = $19,
                 image_generation_succeeded = $20, cost_source = $21,
                 cost_amount = $22::numeric, cost_currency = $23,
                 completed_at = greatest($24, started_at),
                 upstream_transport = coalesce($25, upstream_transport), service_tier = $26,
                 provider_observation_json = $27, error_details = $28,
                 diagnostic_trace_json = $29, billing_snapshot_json = $30,
                 request_observation_json = request_observation_json || $31::jsonb || jsonb_build_object(
                   'transport', ($31::jsonb -> 'transport') || jsonb_strip_nulls(jsonb_build_object(
                     'httpVersion', coalesce($31::jsonb #>> '{transport,httpVersion}', request_observation_json #>> '{transport,httpVersion}'))),
                   'continuation', coalesce(request_observation_json -> 'continuation', '{}'::jsonb)
                     || ($31::jsonb -> 'continuation'))
             where id = $1 and outcome = 'running'
             returning id, client_api_key_ref, continuation_affinity_hash,
                       continuation_requested, provider_kind, upstream_transport,
                       outcome, started_at, completed_at
           ), recovery_target as (
             select prior.id
             from model_requests prior
             cross join finalized current
             where not current.continuation_requested
               and current.continuation_affinity_hash is not null
               and prior.client_api_key_ref = current.client_api_key_ref
               and prior.continuation_affinity_hash = current.continuation_affinity_hash
               and prior.continuation_requested
               and prior.outcome = 'failed'
               and prior.error_kind = 'continuation_recovery_required'
               and prior.recovered_at is null
               and current.started_at >= prior.completed_at
               and current.started_at <= prior.completed_at + interval '30 seconds'
             order by prior.completed_at desc, prior.id desc
             limit 1
           ), recovery_update as (
             update model_requests prior
             set recovery_attempt_count = prior.recovery_attempt_count + 1,
                 recovery_request_id = case
                   when current.outcome = 'succeeded' then current.id
                   else prior.recovery_request_id
                 end,
                 recovered_at = case
                   when current.outcome = 'succeeded' then current.completed_at
                   else prior.recovered_at
                 end,
                 request_observation_json = case when current.outcome = 'succeeded' then
                   prior.request_observation_json || jsonb_build_object('recovery', jsonb_build_object(
                     'retryDelayMs', greatest(0, floor(extract(epoch from (current.started_at - prior.completed_at)) * 1000))::bigint,
                     'totalLatencyMs', greatest(0, floor(extract(epoch from (current.completed_at - prior.completed_at)) * 1000))::bigint))
                   else prior.request_observation_json end
             from finalized current
             join recovery_target target on true
             where prior.id = target.id
               and prior.recovered_at is null
             returning prior.id
           ), session_transport_recovery_target as (
             select prior.id
             from model_requests prior
             cross join finalized current
             where current.outcome = 'succeeded'
               and current.provider_kind = 'openai'
               and current.upstream_transport = 'http_sse'
               and current.continuation_affinity_hash is not null
               and prior.client_api_key_ref = current.client_api_key_ref
               and prior.continuation_affinity_hash = current.continuation_affinity_hash
               and prior.provider_kind = current.provider_kind
               and prior.outcome = 'failed'
               and prior.error_kind = 'upstream_unavailable'
               and prior.upstream_transport = 'websocket'
               and prior.upstream_send_state = 'ambiguous'
               and prior.downstream_committed_at is null
               and prior.recovered_at is null
               and current.started_at >= prior.completed_at
               and current.started_at <= prior.completed_at + interval '30 seconds'
           ), session_transport_recovery_update as (
             update model_requests prior
             set recovery_attempt_count = prior.recovery_attempt_count + 1,
                 recovery_request_id = current.id,
                 recovered_at = current.completed_at,
                 request_observation_json = prior.request_observation_json || jsonb_build_object('recovery', jsonb_build_object(
                   'retryDelayMs', greatest(0, floor(extract(epoch from (current.started_at - prior.completed_at)) * 1000))::bigint,
                   'totalLatencyMs', greatest(0, floor(extract(epoch from (current.completed_at - prior.completed_at)) * 1000))::bigint))
             from finalized current
             join session_transport_recovery_target target on true
             where prior.id = target.id
               and prior.recovered_at is null
             returning prior.id
           )
           select (select count(*) from finalized)::bigint
                + (select count(*) * 0 from recovery_update)::bigint
                + (select count(*) * 0 from session_transport_recovery_update)::bigint",
        )
        .bind(&finalization.model_request_id)
        .bind(finalization.outcome.as_str())
        .bind(finalization.upstream_send_state.as_str())
        .bind(
            i32::try_from(finalization.attempt_count)
                .map_err(|source| invalid("attempt_count is too large").with_source(source))?,
        )
        .bind(finalization.downstream_committed_at)
        .bind(finalization.client_status_code.map(i32::from))
        .bind(finalization.upstream_status_code.map(i32::from))
        .bind(finalization.client_response_id.map(String::into_bytes))
        .bind(finalization.upstream_request_id)
        .bind(finalization.upstream_response_id.map(String::into_bytes))
        .bind(finalization.error_kind)
        .bind(optional_i64(finalization.usage.input_tokens, "input_tokens")?)
        .bind(optional_i64(finalization.usage.output_tokens, "output_tokens")?)
        .bind(optional_i64(finalization.usage.cached_input_tokens, "cached_input_tokens")?)
        .bind(optional_i64(finalization.usage.cache_write_input_tokens, "cache_write_input_tokens")?)
        .bind(optional_i64(finalization.usage.reasoning_output_tokens, "reasoning_output_tokens")?)
        .bind(optional_i64(finalization.usage.image_input_tokens, "image_input_tokens")?)
        .bind(optional_i64(finalization.usage.image_output_tokens, "image_output_tokens")?)
        .bind(optional_i64(finalization.usage.total_tokens, "total_tokens")?)
        .bind(finalization.image_generation_succeeded)
        .bind(finalization.cost_source.as_str())
        .bind(finalization.cost_amount.map(|amount| amount.to_string()))
        .bind(finalization.cost_currency)
        .bind(finalization.completed_at)
        .bind(finalization.upstream_transport)
        .bind(finalization.service_tier)
        .bind(finalization.provider_metadata_json.map(sqlx::types::Json))
        .bind(finalization.error_details)
        .bind(finalization.diagnostic_trace_json.map(sqlx::types::Json))
        .bind(finalization.billing_snapshot_json.map(sqlx::types::Json))
        .bind(observation)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| postgres_unavailable("finalize model request", source))?;
        Ok(finalized == 1)
    }

    async fn recover_expired_model_requests(
        &self,
        now: DateTime<Utc>,
    ) -> StoreResult<ModelRequestRecoveryReport> {
        let result = sqlx::query(
            "with candidates as materialized (
               select request.id from model_requests request
               left join model_request_leases lease on lease.request_id = request.id
               where request.outcome = 'running'
                 and coalesce(lease.lease_expires_at, request.recovery_lease_expires_at, request.started_at) <= $1
               order by request.id for update of request skip locked
             ), expired as materialized (
               select candidate.id from candidates candidate
               left join lateral (
                 select lease_expires_at from model_request_leases
                 where request_id = candidate.id for update
               ) lease on true
               where lease.lease_expires_at is null or lease.lease_expires_at <= $1
             ) update model_requests
             set outcome = 'incomplete', error_kind = 'process_interrupted',
                 request_observation_json = request_observation_json || jsonb_build_object('error',
                   jsonb_build_object('message', case when exists (select 1 from model_request_leases where request_id = model_requests.id)
                     then 'request did not reach a terminal state' else 'request recovery lease is missing' end)),
                 image_generation_succeeded = case
                   when image_generation_requested then false else null
                 end,
                 completed_at = greatest($1, started_at)
             where outcome = 'running' and id in (select id from expired)",
        )
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|source| postgres_unavailable("recover expired model requests", source))?;
        Ok(ModelRequestRecoveryReport {
            requests: result.rows_affected(),
        })
    }
}

#[async_trait]
impl ExecutionStore for PgExecutionStore {
    fn maintain_request(
        &self,
        request_id: &ModelRequestId,
        deadline: gateway_core::lifecycle::Deadline,
    ) -> Box<dyn gateway_core::lifecycle::LeaseGuard> {
        let pool = self.pool.clone();
        let request_id = request_id.as_str().to_owned();
        Box::new(crate::lease_renewal::LeaseRenewal::spawn(
            deadline,
            // 请求记录为 best-effort 观测，续期失败不能取消客户端执行
            None,
            move |ttl| {
                let pool = pool.clone();
                let request_id = request_id.clone();
                Box::pin(async move {
                    let ttl_ms = i64::try_from(ttl.as_millis()).map_err(|_| {
                        crate::StoreError::InvalidData {
                            source: None,
                            entity: "model request",
                            message: "lease TTL is invalid".to_owned(),
                        }
                    })?;
                    // 首次观测可能尚在队列中；缺行不创建记录，已终结行不改写
                    sqlx::query_scalar::<_, bool>(
                        "with renewed as (
                           update model_request_leases set lease_expires_at = now() + $2 * interval '1 millisecond'
                           where request_id = $1 and lease_expires_at > now()
                           returning request_id
                         )
                         select exists(select 1 from renewed)
                           or not exists(select 1 from model_requests where id = $1 and outcome = 'running')",
                    )
                    .bind(&request_id)
                    .bind(ttl_ms)
                    .fetch_one(&pool)
                    .await
                    .map_err(|source| postgres_unavailable("renew model request recovery lease", source))
                })
            },
        ))
    }

    async fn create_model_request(
        &self,
        request: CoreNewModelRequest,
    ) -> Result<(), CoreStoreError> {
        self.insert_model_request(new_model_request_row(request))
            .await
            .map_err(core_store_error)
    }

    async fn record_attempt(&self, attempt: CoreAttemptRecord) -> Result<(), CoreStoreError> {
        let expected_count = attempt.attempt_count.get();
        let persisted = self
            .begin_model_request_attempt(attempt_start_row(attempt))
            .await
            .map_err(core_store_error)?;
        if persisted == expected_count {
            Ok(())
        } else {
            Err(CoreStoreError::new(CoreStoreErrorKind::InvalidState))
        }
    }

    async fn create_model_request_with_attempt(
        &self,
        request: CoreNewModelRequest,
        attempt: CoreAttemptRecord,
    ) -> Result<(), CoreStoreError> {
        // 合并写只对首次 attempt 成立（插入即带 attempt 列）；其余走基础两步
        if attempt.attempt_count.get() != 1 {
            self.create_model_request(request).await?;
            return self.record_attempt(attempt).await;
        }
        self.insert_model_request_with_first_attempt(
            new_model_request_row(request),
            attempt_start_row(attempt),
        )
        .await
        .map_err(core_store_error)
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
            .map_err(|source| CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source))?;
        super::OpsEventRepository::append_ops_event(
            &super::PgOpsEventRepository::new(self.pool.clone()),
            super::OpsEvent {
                id: Uuid::now_v7().to_string(),
                model_request_id: Some(failure.request_id.as_str().to_owned()),
                attempt_index: Some(failure.attempt_index.get()),
                level: super::OpsEventLevel::Warning,
                component: "routing".to_owned(),
                operation: failure.trigger.as_str().to_owned(),
                provider_kind: Some(failure.provider_kind.as_str().to_owned()),
                account_snapshot: failure.account_snapshot,
                provider_account_ref: failure.account_id.as_ref().map(|id| id.as_str().to_owned()),
                upstream_model_id: failure
                    .upstream_model_id
                    .as_ref()
                    .map(|model| model.as_str().to_owned()),
                failure_kind: error.kind().as_str().to_owned(),
                upstream_send_state: Some(error.send_state().as_str().to_owned()),
                error_details: error.error_details(),
                status_code: error.upstream_status().or(failure.upstream_status_code),
                provider_error_code: error.upstream_code().map(|code| code.as_str().to_owned()),
                retry_after_ms,
                upstream_request_id: error
                    .upstream_request_id()
                    .map(|id| id.as_str().to_owned())
                    .or(failure.upstream_request_id),
                latency_ms: Some(
                    u64::try_from(failure.latency.as_millis()).map_err(|source| {
                        CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source)
                    })?,
                ),
                message: error.diagnostic().map_or_else(
                    || "intermediate upstream failure".to_owned(),
                    |diagnostic| diagnostic.as_str().to_owned(),
                ),
                created_at: Utc::now(),
            },
        )
        .await
        .map_err(core_store_error)
    }

    async fn record_entry_rejection(
        &self,
        rejection: gateway_core::engine::EntryRejection,
    ) -> Result<(), CoreStoreError> {
        let error = rejection.error;
        super::OpsEventRepository::append_ops_event(
            &super::PgOpsEventRepository::new(self.pool.clone()),
            super::OpsEvent {
                id: Uuid::now_v7().to_string(),
                model_request_id: None,
                attempt_index: None,
                level: super::OpsEventLevel::Warning,
                component: "request_entry".to_owned(),
                operation: "reject".to_owned(),
                provider_kind: None,
                account_snapshot: None,
                provider_account_ref: None,
                upstream_model_id: None,
                failure_kind: error.kind().as_str().to_owned(),
                upstream_send_state: None,
                error_details: error
                    .error_details()
                    .map(gateway_core::error::ErrorDetails::into_string),
                status_code: None,
                provider_error_code: error.client_error_code().map(str::to_owned),
                retry_after_ms: error
                    .retry_after()
                    .and_then(|delay| u64::try_from(delay.as_millis()).ok()),
                upstream_request_id: None,
                latency_ms: u64::try_from(rejection.latency.as_millis()).ok(),
                // 入口尚无 model_requests 行，关联 ID 放在安全消息中，不能伪造外键
                message: serde_json::json!({
                    "requestId": rejection.request_id.as_str(),
                    "clientKeyId": rejection.client_key_id.as_str(),
                    "message": error.client_message(),
                })
                .to_string(),
                created_at: Utc::now(),
            },
        )
        .await
        .map_err(core_store_error)
    }

    async fn record_probe_failure(&self, failure: ProbeFailure) -> Result<(), CoreStoreError> {
        let error = failure.error;
        let retry_after_ms = error
            .retry_after()
            .map(|duration| u64::try_from(duration.as_millis()))
            .transpose()
            .map_err(|source| CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source))?;
        let latency_ms = u64::try_from(failure.latency.as_millis())
            .map_err(|source| CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source))?;
        let provider_error_code = error.upstream_code().map(|code| code.as_str().to_owned());
        super::OpsEventRepository::append_ops_event(
            &super::PgOpsEventRepository::new(self.pool.clone()),
            super::OpsEvent {
                id: Uuid::now_v7().to_string(),
                model_request_id: None,
                attempt_index: None,
                level: super::OpsEventLevel::Warning,
                component: "account_probe".to_owned(),
                operation: "connection_test".to_owned(),
                provider_kind: Some(failure.provider_kind.as_str().to_owned()),
                account_snapshot: None,
                provider_account_ref: Some(failure.account_id.as_str().to_owned()),
                upstream_model_id: Some(failure.upstream_model_id.as_str().to_owned()),
                failure_kind: error.kind().as_str().to_owned(),
                upstream_send_state: Some(error.send_state().as_str().to_owned()),
                error_details: error.error_details(),
                status_code: error.upstream_status(),
                provider_error_code,
                retry_after_ms,
                upstream_request_id: error.upstream_request_id().map(|id| id.as_str().to_owned()),
                latency_ms: Some(latency_ms),
                message: error.diagnostic().map_or_else(
                    || "account connection test failed".to_owned(),
                    |diagnostic| diagnostic.as_str().to_owned(),
                ),
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
        let continuation_unavailable_reason = finalization
            .failure_observation
            .continuation_unavailable_reason
            .clone();
        let connection_observation = finalization
            .failure_observation
            .upstream_connection
            .as_ref();
        let upstream_connection_id =
            connection_observation.map(|observation| observation.connection_id().to_owned());
        let upstream_connection_exit_reason =
            connection_observation.map(|observation| observation.exit_reason().to_owned());
        let upstream_connection_age_ms =
            connection_observation.map(|observation| observation.age_ms());
        let upstream_connection_idle_ms =
            connection_observation.map(|observation| observation.idle_ms());
        let provider_metadata_json = finalization
            .provider_metadata_json
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|source| CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source))?;
        if provider_metadata_json
            .as_ref()
            .is_some_and(|metadata| !metadata.is_object())
        {
            return Err(CoreStoreError::new(CoreStoreErrorKind::InvalidData));
        }
        let (cost_source, cost_amount, cost_currency) = match finalization.cost.total() {
            Some(total) => (
                cost_source_from_core(finalization.cost.source()),
                Some(
                    total
                        .amount()
                        .to_string()
                        .parse::<DecimalAmount>()
                        .map_err(|source| {
                            CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source)
                        })?,
                ),
                Some(total.currency().as_str().to_owned()),
            ),
            None => (CostSource::Unavailable, None, None),
        };
        let error_kind = if continuation_unavailable_reason.is_some() {
            Some(
                ProviderErrorKind::ContinuationRecoveryRequired
                    .as_str()
                    .to_owned(),
            )
        } else {
            finalization
                .error
                .as_ref()
                .map(|error| error.kind().as_str().to_owned())
        };
        let error_message = finalization.error.as_ref().map(|error| {
            error.diagnostic().map_or_else(
                || error.safe_message().to_owned(),
                |diagnostic| diagnostic.as_str().to_owned(),
            )
        });
        let completed = ModelRequestRepository::finalize_model_request(
            self,
            ModelRequestFinalization {
                billing_snapshot_json: finalization
                    .cost
                    .breakdown()
                    .map(super::pricing::encode_billing_snapshot),
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
                websocket_pool: finalization.websocket_pool,
                service_tier: finalization.service_tier,
                upstream_response_model: finalization.upstream_response_model,
                provider_metadata_json,
                diagnostic_trace_json: finalization
                    .diagnostic_trace_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|source| {
                        CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source)
                    })?,
                error_kind,
                provider_error_code: finalization.provider_error_code,
                error_message,
                error_details: finalization.error_details,
                continuation_unavailable_reason,
                upstream_connection_id,
                upstream_connection_exit_reason,
                upstream_connection_age_ms,
                upstream_connection_idle_ms,
                retry_after_ms: finalization.retry_after_ms,
                usage: finalization.usage,
                image_generation_succeeded: finalization.image_generation_succeeded,
                cost_source,
                cost_amount,
                cost_currency,
                timings: finalization.timings,
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

fn new_model_request_row(request: CoreNewModelRequest) -> NewModelRequest {
    let (routing_scope, routing_group_refs, routing_group_names_snapshot) =
        routing_snapshot_row(&request.routing);
    NewModelRequest {
        id: request.id.as_str().to_owned(),
        client_api_key_name_snapshot: request.client_api_key_name_snapshot,
        client_api_key_ref: request.client_api_key_ref.as_str().to_owned(),
        config_revision: request.config_revision.get(),
        routing_scope,
        routing_group_refs,
        routing_group_names_snapshot,
        protocol: request.protocol,
        operation: request.operation.as_str().to_owned(),
        endpoint: request.endpoint,
        client_transport: request.client_transport,
        requested_model_id: request
            .requested_model
            .map(|model| model.as_str().to_owned()),
        client_ip: request.client_ip.map(|address| address.to_string()),
        user_agent: request.user_agent,
        reasoning_effort: request.reasoning_effort,
        reasoning_preset: request.reasoning_preset,
        request_kind: request.request_kind,
        subagent_kind: request.subagent_kind,
        compact: request.compact,
        continuation: ContinuationRequestObservation {
            affinity_hash: request.continuation.affinity_hash,
            previous_response_id_hash: request.continuation.previous_response_id_hash,
            requested: request.continuation.requested,
        },
        image_generation_requested: request.image_generation_requested,
        admission_decision_ms: request.admission_decision_ms,
        started_at: DateTime::<Utc>::from(request.started_at),
        lease_expires_at: DateTime::<Utc>::from(request.deadline_at.lease_deadline()),
    }
}

fn routing_snapshot_row(snapshot: &AccountRoutingSnapshot) -> (String, Vec<String>, Value) {
    let groups = snapshot.groups_snapshot();
    (
        snapshot.kind().as_str().to_owned(),
        groups
            .iter()
            .map(|group| group.id().as_str().to_owned())
            .collect(),
        Value::Array(
            groups
                .iter()
                .map(|group| Value::String(group.name().to_owned()))
                .collect(),
        ),
    )
}

fn validate_routing_snapshot(scope: &str, refs: &[String], names: &Value) -> StoreResult<()> {
    let Some(names) = names.as_array() else {
        return Err(invalid("routing group names snapshot must be an array"));
    };
    let valid = match scope {
        value if value == AccountRoutingScopeKind::All.as_str() => {
            refs.is_empty() && names.is_empty()
        }
        value if value == AccountRoutingScopeKind::Groups.as_str() => {
            !refs.is_empty()
                && refs.len() == names.len()
                && refs.iter().all(|id| !id.is_empty())
                && names
                    .iter()
                    .all(|name| name.as_str().is_some_and(|name| !name.is_empty()))
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid("invalid routing scope snapshot"))
    }
}

fn attempt_start_row(attempt: CoreAttemptRecord) -> ModelRequestAttemptStart {
    ModelRequestAttemptStart {
        model_request_id: attempt.request_id.as_str().to_owned(),
        attempt_count: attempt.attempt_count.get(),
        provider_kind: attempt.provider_kind.as_str().to_owned(),
        account_snapshot: attempt.account_snapshot,
        provider_account_ref: attempt
            .provider_account_ref
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        upstream_model_id: attempt
            .upstream_model_id
            .map(|model| model.as_str().to_owned()),
        upstream_transport: attempt.upstream_transport,
        http_version: attempt.http_version,
        account_selection_wait_ms: attempt.account_selection_wait_ms,
        capacity_used_slots: attempt.capacity_used_slots,
        capacity_total_slots: attempt.capacity_total_slots,
    }
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

fn validate_optional_stable_reason(value: Option<&str>, field: &str) -> StoreResult<()> {
    if value.is_some_and(|value| {
        value.is_empty()
            || value.len() > 64
            || !value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
            })
    }) {
        return Err(invalid(field));
    }
    Ok(())
}

fn validate_connection_observation(finalization: &ModelRequestFinalization) -> StoreResult<()> {
    let fields_present = [
        finalization.upstream_connection_id.is_some(),
        finalization.upstream_connection_exit_reason.is_some(),
        finalization.upstream_connection_age_ms.is_some(),
        finalization.upstream_connection_idle_ms.is_some(),
    ];
    if fields_present.iter().any(|present| *present)
        && !fields_present.iter().all(|present| *present)
    {
        return Err(invalid("upstream connection observation is incomplete"));
    }
    if let Some(connection_id) = finalization.upstream_connection_id.as_deref()
        && (connection_id.is_empty()
            || connection_id.len() > 128
            || connection_id.chars().any(char::is_control))
    {
        return Err(invalid("upstream connection ID is invalid"));
    }
    validate_optional_stable_reason(
        finalization.upstream_connection_exit_reason.as_deref(),
        "upstream connection exit reason is invalid",
    )?;
    if finalization
        .upstream_connection_age_ms
        .zip(finalization.upstream_connection_idle_ms)
        .is_some_and(|(age, idle)| idle > age)
    {
        return Err(invalid("upstream connection idle time exceeds its age"));
    }
    Ok(())
}

fn to_i64(value: u64, field: &'static str) -> StoreResult<i64> {
    i64::try_from(value).map_err(|source| invalid(field).with_source(source))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: ENTITY,
        message: message.to_owned(),
    }
}

#[async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for PgExecutionStore {
    async fn record_failure(
        &self,
        failure: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), CoreStoreError> {
        use super::{OpsEvent, OpsEventLevel, OpsEventRepository, PgOpsEventRepository};
        let event = OpsEvent {
            id: Uuid::now_v7().to_string(),
            model_request_id: None,
            attempt_index: None,
            level: OpsEventLevel::Warning,
            component: failure.component.to_owned(),
            operation: failure.operation.to_owned(),
            provider_kind: failure.provider_kind.map(|kind| kind.as_str().to_owned()),
            account_snapshot: failure.account_snapshot,
            provider_account_ref: failure.account_id.map(|id| id.as_str().to_owned()),
            upstream_model_id: None,
            failure_kind: failure.kind.to_owned(),
            upstream_send_state: None,
            error_details: failure
                .details
                .map(gateway_core::error::ErrorDetails::into_string),
            status_code: failure.upstream_status,
            provider_error_code: failure.upstream_code.map(|code| code.as_str().to_owned()),
            retry_after_ms: None,
            upstream_request_id: None,
            latency_ms: None,
            message: serde_json::json!({
                "correlationId": failure.correlation_id,
                "message": failure.message,
            })
            .to_string(),
            created_at: failure.occurred_at.into(),
        };
        PgOpsEventRepository::new(self.pool.clone())
            .append_ops_event(event)
            .await
            .map_err(core_store_error)
    }
}
