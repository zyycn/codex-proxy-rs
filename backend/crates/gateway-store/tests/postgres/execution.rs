use std::time::{Duration as StdDuration, SystemTime};

use chrono::{DateTime, Duration, Utc};
use futures::TryStreamExt as _;
use gateway_admin::{
    model::{PageSize, observability as admin_observability},
    ports::store::ObservabilityStore as _,
};
use gateway_core::diagnostics::TraceContext;
use gateway_core::engine::{
    ExecutionOutcome, ExecutionStore, ModelRequestFailureObservation,
    ModelRequestFinalization as CoreModelRequestFinalization, ModelRequestId,
    ModelRequestTimings as CoreModelRequestTimings, NewModelRequest as CoreNewModelRequest,
};
use gateway_core::error::{
    GatewayError, GatewayErrorKind, ProviderConnectionObservation, StoreErrorKind,
};
use gateway_core::metering::{CalculatedCost, CostEstimate, Usage};
use gateway_core::operation::OperationKind;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{AccountRoutingSnapshot, ConfigRevision, PublicModelId};
use gateway_core::upstream::UpstreamSendState;
use gateway_store::postgres::{
    AttemptMetrics, ModelRequestAttemptStart, ModelRequestRepository, NewModelRequest,
    ObservabilityPageSize, ObservabilityRange, ObservabilityRepository, OpsErrorFilter,
    OpsErrorQuery, PgExecutionStore, UsageRecordFilter, UsageRecordQuery,
};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{TestDatabase, admin_observability_store, observability_repository};

#[test]
fn postgres_execution_adapter_implements_core_port() {
    fn assert_port<T: ExecutionStore>() {}
    assert_port::<PgExecutionStore>();
}

#[test]
fn model_request_rejects_mismatched_client_key_live_id() {
    let started_at = Utc::now();
    let request = NewModelRequest {
        admission_decision_ms: None,
        id: "request-1".to_owned(),
        client_api_key_id: Some("key-live".to_owned()),
        client_api_key_ref: "key-history".to_owned(),
        config_revision: 1,
        protocol: "openai".to_owned(),
        operation: "responses".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: Some("coding".to_owned()),
        routing_scope: "all".to_owned(),
        routing_group_refs: Vec::new(),
        routing_group_names_snapshot: serde_json::json!([]),
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        request_kind: None,
        subagent_kind: None,
        compact: false,
        continuation: Default::default(),
        image_generation_requested: false,
        started_at,
        deadline_at: started_at + Duration::seconds(30),
    };
    assert!(request.validate().is_err());
}

#[tokio::test]
async fn merged_model_less_first_attempt_should_match_sequential_semantics() {
    let Some(database) = TestDatabase::create("execution_merged_insert").await else {
        return;
    };
    let repository = PgExecutionStore::new(database.pool.clone());
    let started_at = Utc::now();
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind,
           provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, enabled, credential_state,
           credential_observed_at, created_at, updated_at
         ) values (
           'acct_merged', 'openai', 'merged', null, 'user-merged', null, null, 'oauth',
           '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', $1, $1, $1
         )",
    )
    .bind(started_at)
    .execute(&database.pool)
    .await
    .expect("seed provider account");
    let request = NewModelRequest {
        admission_decision_ms: None,
        id: "req_merged".to_owned(),
        client_api_key_id: None,
        client_api_key_ref: "key_merged".to_owned(),
        config_revision: 1,
        protocol: "openai".to_owned(),
        operation: "generate_image".to_owned(),
        endpoint: "/v1/images/generations".to_owned(),
        client_transport: "http_json".to_owned(),
        requested_model_id: None,
        routing_scope: "all".to_owned(),
        routing_group_refs: Vec::new(),
        routing_group_names_snapshot: serde_json::json!([]),
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        request_kind: None,
        subagent_kind: None,
        compact: false,
        continuation: Default::default(),
        image_generation_requested: false,
        started_at,
        deadline_at: started_at + Duration::seconds(30),
    };
    let attempt = ModelRequestAttemptStart {
        account_selection_wait_ms: None,
        capacity_used_slots: None,
        capacity_total_slots: None,
        model_request_id: "req_merged".to_owned(),
        attempt_count: 1,
        provider_kind: "openai".to_owned(),
        provider_account_id: Some("acct_merged".to_owned()),
        provider_account_ref: Some("acct_merged".to_owned()),
        upstream_model_id: None,
        upstream_transport: "http_json".to_owned(),
        http_version: None,
    };

    repository
        .insert_model_request_with_first_attempt(request, attempt)
        .await
        .expect("merged insert");

    let (attempt_count, send_state, provider_kind, outcome, requested_model, upstream_model): (
        i32,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "select attempt_count, upstream_send_state, provider_kind, outcome,
                    requested_model_id, upstream_model_id
             from model_requests where id = 'req_merged'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load merged request");
    assert_eq!(
        (
            attempt_count,
            send_state.as_str(),
            provider_kind.as_str(),
            outcome.as_str(),
            requested_model,
            upstream_model,
        ),
        (1, "not_sent", "openai", "running", None, None)
    );

    // 后续 attempt 沿用常规 CAS 递增路径；已持久化的 sent 水位不被重试重置。
    repository
        .mark_upstream_send_state(
            "req_merged",
            gateway_store::postgres::UpstreamSendState::Sent,
        )
        .await
        .expect("mark sent before retry");
    let second = repository
        .begin_model_request_attempt(ModelRequestAttemptStart {
            account_selection_wait_ms: None,
            capacity_used_slots: None,
            capacity_total_slots: None,
            model_request_id: "req_merged".to_owned(),
            attempt_count: 2,
            provider_kind: "openai".to_owned(),
            provider_account_id: Some("acct_merged".to_owned()),
            provider_account_ref: Some("acct_merged".to_owned()),
            upstream_model_id: None,
            upstream_transport: "http_json".to_owned(),
            http_version: None,
        })
        .await
        .expect("second attempt");
    assert_eq!(second, 2);
    let send_state: String = sqlx::query_scalar(
        "select upstream_send_state from model_requests where id = 'req_merged'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load send state after retry");
    assert_eq!(send_state, "sent");
    database.close().await;
}

#[tokio::test]
async fn model_request_persists_group_routing_snapshot_without_live_group_foreign_keys() {
    let Some(database) = TestDatabase::create("execution_group_routing_history").await else {
        return;
    };
    let repository = PgExecutionStore::new(database.pool.clone());
    let started_at = Utc::now();
    repository
        .insert_model_request(NewModelRequest {
            admission_decision_ms: None,
            id: "req_group_history".to_owned(),
            client_api_key_id: None,
            client_api_key_ref: "key_group_history".to_owned(),
            config_revision: 7,
            routing_scope: "groups".to_owned(),
            routing_group_refs: vec![
                "grp_00000000000000000000000000000001".to_owned(),
                "grp_00000000000000000000000000000002".to_owned(),
            ],
            routing_group_names_snapshot: serde_json::json!(["Production Pool", "Overflow Pool"]),
            protocol: "openai".to_owned(),
            operation: "responses".to_owned(),
            endpoint: "/v1/responses".to_owned(),
            client_transport: "http_sse".to_owned(),
            requested_model_id: Some("coding".to_owned()),
            client_ip: None,
            user_agent: None,
            reasoning_effort: None,
            reasoning_preset: None,
            request_kind: None,
            subagent_kind: None,
            compact: false,
            continuation: Default::default(),
            image_generation_requested: false,
            started_at,
            deadline_at: started_at + Duration::seconds(30),
        })
        .await
        .expect("insert grouped request history");

    let stored: (String, Vec<String>, serde_json::Value) = sqlx::query_as(
        "select routing_scope, routing_group_refs, routing_group_names_snapshot
         from model_requests where id = 'req_group_history'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load grouped request history");
    assert_eq!(stored.0, "groups");
    assert_eq!(
        stored.1,
        [
            "grp_00000000000000000000000000000001",
            "grp_00000000000000000000000000000002",
        ]
    );
    assert_eq!(
        stored.2,
        serde_json::json!(["Production Pool", "Overflow Pool"])
    );
    database.close().await;
}

#[tokio::test]
async fn downstream_commit_should_atomically_record_http_status_once() {
    let Some(database) = TestDatabase::create("execution_commit_status").await else {
        return;
    };
    seed_running_request(&database.pool, "req_commit_status")
        .await
        .expect("seed model request");
    let repository = PgExecutionStore::new(database.pool.clone());

    let committed = ModelRequestRepository::mark_downstream_committed(
        &repository,
        "req_commit_status",
        Utc::now(),
        Some(200),
    )
    .await
    .expect("commit downstream");
    let overwritten = repository
        .record_client_status_code("req_commit_status", 500)
        .await
        .expect("reject status overwrite without store failure");
    let (committed_at, status): (Option<chrono::DateTime<Utc>>, Option<i32>) = sqlx::query_as(
        "select downstream_committed_at, client_status_code
         from model_requests where id = 'req_commit_status'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load committed request");

    assert!(committed);
    assert!(!overwritten);
    assert!(committed_at.is_some());
    assert_eq!(status, Some(200));
    database.close().await;
}

#[tokio::test]
async fn terminal_failure_should_accept_exactly_one_client_status_backfill() {
    let Some(database) = TestDatabase::create("execution_terminal_status").await else {
        return;
    };
    seed_running_request(&database.pool, "req_terminal_status")
        .await
        .expect("seed model request");
    sqlx::query(
        "update model_requests
         set outcome = 'failed', completed_at = now()
         where id = 'req_terminal_status'",
    )
    .execute(&database.pool)
    .await
    .expect("finalize model request");
    let repository = PgExecutionStore::new(database.pool.clone());

    let recorded = repository
        .record_client_status_code("req_terminal_status", 429)
        .await
        .expect("record terminal client status");
    let overwritten = repository
        .record_client_status_code("req_terminal_status", 500)
        .await
        .expect("reject terminal status overwrite without store failure");
    let status: Option<i32> = sqlx::query_scalar(
        "select client_status_code from model_requests where id = 'req_terminal_status'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load terminal client status");

    assert!(recorded);
    assert!(!overwritten);
    assert_eq!(status, Some(429));
    database.close().await;
}

#[tokio::test]
async fn core_adapter_should_persist_calculated_cost_exactly() {
    let Some(database) = TestDatabase::create("execution_calculated_cost").await else {
        return;
    };
    seed_running_request(&database.pool, "req_calculated_cost")
        .await
        .expect("seed model request");
    sqlx::query(
        "update model_requests
         set provider_kind = 'xai', provider_account_ref = 'acct_xai',
             upstream_model_id = 'grok-4.5', upstream_transport = 'http_sse', attempt_count = 1
         where id = 'req_calculated_cost'",
    )
    .execute(&database.pool)
    .await
    .expect("seed model request attempt");
    let repository = PgExecutionStore::new(database.pool.clone());

    let mut finalization = successful_core_finalization("req_calculated_cost");
    finalization.websocket_pool = Some("reuse".to_owned());
    finalization.service_tier = Some("priority".to_owned());
    finalization.cost = CalculatedCost::from_usd_ticks(12_345)
        .expect("calculated cost")
        .into_estimate();
    ExecutionStore::finalize_model_request(&repository, finalization)
        .await
        .expect("persist calculated cost");
    let persisted: (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "select cost_source, cost_amount::text, cost_currency, upstream_transport, http_version,
                websocket_pool, service_tier
         from model_requests where id = 'req_calculated_cost'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load calculated cost");

    assert_eq!(
        persisted,
        (
            "calculated".to_owned(),
            "0.0000012345".to_owned(),
            "USD".to_owned(),
            "websocket".to_owned(),
            "HTTP/2".to_owned(),
            "reuse".to_owned(),
            Some("priority".to_owned()),
        )
    );
    database.close().await;
}

#[tokio::test]
async fn core_adapter_should_persist_image_result_and_new_websocket_pool() {
    let Some(database) = TestDatabase::create("execution_image_usage").await else {
        return;
    };
    seed_running_request(&database.pool, "req_image_usage")
        .await
        .expect("seed image request");
    sqlx::query(
        "update model_requests
         set image_generation_requested = true, provider_kind = 'openai',
             provider_account_ref = 'acct_openai', upstream_model_id = 'gpt-image',
             upstream_transport = 'websocket', attempt_count = 1
         where id = 'req_image_usage'",
    )
    .execute(&database.pool)
    .await
    .expect("seed image attempt");
    let repository = PgExecutionStore::new(database.pool.clone());
    let mut usage = Usage::new();
    usage.image_input_tokens = Some(31);
    usage.image_output_tokens = Some(9);
    let mut finalization = successful_core_finalization("req_image_usage");
    finalization.usage = usage;
    finalization.image_generation_succeeded = Some(true);
    finalization.websocket_pool = Some("new".to_owned());

    ExecutionStore::finalize_model_request(&repository, finalization)
        .await
        .expect("persist image usage");
    let persisted: (bool, Option<i64>, Option<i64>, Option<bool>, Option<String>) = sqlx::query_as(
        "select image_generation_requested, image_input_tokens, image_output_tokens,
                    image_generation_succeeded, websocket_pool
             from model_requests where id = 'req_image_usage'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load image usage");

    assert_eq!(
        persisted,
        (true, Some(31), Some(9), Some(true), Some("new".to_owned()))
    );
    database.close().await;
}

#[tokio::test]
async fn expired_image_request_should_be_recovered_as_failed() {
    let Some(database) = TestDatabase::create("execution_expired_image").await else {
        return;
    };
    seed_running_request(&database.pool, "req_expired_image")
        .await
        .expect("seed image request");
    sqlx::query(
        "update model_requests
         set image_generation_requested = true,
             started_at = now() - interval '2 seconds',
             deadline_at = now() - interval '1 second'
         where id = 'req_expired_image'",
    )
    .execute(&database.pool)
    .await
    .expect("expire image request");
    let repository = PgExecutionStore::new(database.pool.clone());

    repository
        .recover_expired_model_requests(Utc::now())
        .await
        .expect("recover expired image request");
    let persisted: (String, Option<bool>) = sqlx::query_as(
        "select outcome, image_generation_succeeded
         from model_requests where id = 'req_expired_image'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load recovered image request");

    assert_eq!(persisted, ("incomplete".to_owned(), Some(false)));
    database.close().await;
}

fn successful_core_finalization(id: &str) -> CoreModelRequestFinalization {
    CoreModelRequestFinalization {
        diagnostic_trace_json: None,
        request_id: ModelRequestId::new(id).expect("request id"),
        outcome: ExecutionOutcome::Succeeded,
        send_state: UpstreamSendState::Sent,
        attempt_count: 1,
        downstream_committed_at: None,
        client_status_code: Some(200),
        client_response_id: None,
        upstream_status_code: Some(200),
        upstream_request_id: None,
        upstream_response_id: None,
        upstream_transport: Some("websocket".to_owned()),
        http_version: Some("HTTP/2".to_owned()),
        websocket_pool: None,
        service_tier: None,
        provider_metadata_json: None,
        error: None,
        provider_error_code: None,
        raw_upstream_error: None,
        failure_observation: Default::default(),
        retry_after_ms: None,
        usage: Usage::new(),
        image_generation_succeeded: None,
        cost: CostEstimate::unavailable(),
        timings: CoreModelRequestTimings::default(),
        completed_at: std::time::SystemTime::now(),
    }
}

#[tokio::test]
async fn core_adapter_persists_opaque_response_ids_as_bytes() {
    let Some(database) = TestDatabase::create("execution_opaque_response_id").await else {
        return;
    };
    seed_running_request(&database.pool, "req_opaque_response_id")
        .await
        .expect("seed model request");
    let store = PgExecutionStore::new(database.pool.clone());
    let response_id = format!("resp_{}\0opaque", "x".repeat(4_096));
    let mut finalization = successful_core_finalization("req_opaque_response_id");
    finalization.client_response_id = Some(response_id.clone());
    finalization.upstream_response_id = Some(response_id.clone());

    ExecutionStore::finalize_model_request(&store, finalization)
        .await
        .expect("persist opaque response IDs");

    let persisted: (Option<Vec<u8>>, Option<Vec<u8>>) = sqlx::query_as(
        "select client_response_id, upstream_response_id
         from model_requests where id = 'req_opaque_response_id'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load opaque response IDs");
    assert_eq!(persisted.0.as_deref(), Some(response_id.as_bytes()));
    assert_eq!(persisted.1.as_deref(), Some(response_id.as_bytes()));

    database.close().await;
}

#[tokio::test]
async fn core_adapter_persists_raw_upstream_error_verbatim() {
    let Some(database) = TestDatabase::create("execution_raw_upstream_error").await else {
        return;
    };
    seed_running_request(&database.pool, "req_raw_upstream_error")
        .await
        .expect("seed model request");
    let store = PgExecutionStore::new(database.pool.clone());
    let raw = r#"{"error":{"message":"raw upstream body","opaque":"\u0000"}}"#;
    let mut finalization = successful_core_finalization("req_raw_upstream_error");
    finalization.outcome = ExecutionOutcome::Failed;
    finalization.client_status_code = Some(502);
    finalization.upstream_status_code = Some(500);
    finalization.error = Some(GatewayError::new(
        GatewayErrorKind::UpstreamUnavailable,
        "upstream service is unavailable",
    ));
    finalization.raw_upstream_error = Some(raw.to_owned());

    ExecutionStore::finalize_model_request(&store, finalization)
        .await
        .expect("persist raw upstream error");

    let persisted: Option<String> = sqlx::query_scalar(
        "select raw_upstream_error from model_requests where id = 'req_raw_upstream_error'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load raw upstream error");
    assert_eq!(persisted.as_deref(), Some(raw));

    database.close().await;
}

#[tokio::test]
async fn core_adapter_should_persist_only_object_provider_observation() {
    let Some(database) = TestDatabase::create("execution_provider_observation").await else {
        return;
    };
    seed_running_request(&database.pool, "req_provider_observation")
        .await
        .expect("seed model request");
    let store = PgExecutionStore::new(database.pool.clone());
    let mut finalization = successful_core_finalization("req_provider_observation");
    finalization.provider_metadata_json = Some("{\"effectiveModel\":\"gpt-test\"}".to_owned());

    ExecutionStore::finalize_model_request(&store, finalization)
        .await
        .expect("persist provider observation");
    let persisted: serde_json::Value = sqlx::query_scalar(
        "select provider_observation_json from model_requests where id = 'req_provider_observation'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load provider observation");
    assert_eq!(persisted["effectiveModel"], "gpt-test");

    database.close().await;
}

#[tokio::test]
async fn core_adapter_should_reject_non_object_provider_observation() {
    let Some(database) = TestDatabase::create("execution_invalid_provider_observation").await
    else {
        return;
    };
    seed_running_request(&database.pool, "req_invalid_provider_observation")
        .await
        .expect("seed model request");
    let store = PgExecutionStore::new(database.pool.clone());
    let mut finalization = successful_core_finalization("req_invalid_provider_observation");
    finalization.provider_metadata_json = Some("[]".to_owned());

    assert!(
        ExecutionStore::finalize_model_request(&store, finalization)
            .await
            .is_err()
    );

    database.close().await;
}

#[tokio::test]
async fn core_adapter_persists_continuation_connection_failure_observation() {
    let Some(database) = TestDatabase::create("execution_continuation_observation").await else {
        return;
    };
    seed_running_request(&database.pool, "req_continuation_observation")
        .await
        .expect("seed model request");
    let store = PgExecutionStore::new(database.pool.clone());
    let mut finalization = successful_core_finalization("req_continuation_observation");
    finalization.outcome = ExecutionOutcome::Failed;
    finalization.client_status_code = Some(400);
    finalization.error = Some(GatewayError::new(
        GatewayErrorKind::InvalidRequest,
        "conversation continuation must be rebuilt",
    ));
    finalization.failure_observation = ModelRequestFailureObservation {
        continuation_unavailable_reason: Some("reused_connection_lost".to_owned()),
        upstream_connection: Some(ProviderConnectionObservation::new(
            "connection-observed",
            "tcp_reset",
            12_000,
            4_000,
        )),
    };

    ExecutionStore::finalize_model_request(&store, finalization)
        .await
        .expect("persist continuation failure observation");

    type PersistedContinuationObservation = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    );
    let persisted: PersistedContinuationObservation = sqlx::query_as(
        "select error_kind, continuation_unavailable_reason,
                upstream_connection_id, upstream_connection_exit_reason,
                upstream_connection_age_ms, upstream_connection_idle_ms
           from model_requests where id = 'req_continuation_observation'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load continuation failure observation");
    assert_eq!(
        persisted.0.as_deref(),
        Some("continuation_recovery_required")
    );
    assert_eq!(persisted.1.as_deref(), Some("reused_connection_lost"));
    assert_eq!(persisted.2.as_deref(), Some("connection-observed"));
    assert_eq!(persisted.3.as_deref(), Some("tcp_reset"));
    assert_eq!(persisted.4, Some(12_000));
    assert_eq!(persisted.5, Some(4_000));

    database.close().await;
}

#[tokio::test]
async fn successful_new_chain_should_mark_recent_continuation_failure_recovered() {
    let Some(database) = TestDatabase::create("execution_continuation_recovered").await else {
        return;
    };
    let affinity_hash = "a".repeat(64);
    let failure_completed_at = Utc::now() - Duration::seconds(8);
    seed_continuation_failure(
        &database.pool,
        "req_continuation_failed",
        &affinity_hash,
        failure_completed_at,
    )
    .await;
    seed_recovery_request(
        &database.pool,
        "req_continuation_recovery",
        &affinity_hash,
        failure_completed_at + Duration::seconds(4),
    )
    .await;

    let store = PgExecutionStore::new(database.pool.clone());
    ExecutionStore::finalize_model_request(
        &store,
        successful_core_finalization("req_continuation_recovery"),
    )
    .await
    .expect("finalize successful recovery request");

    type PersistedRecoveryObservation = (
        Option<String>,
        Option<chrono::DateTime<Utc>>,
        i32,
        Option<i64>,
        Option<i64>,
    );
    let recovered: PersistedRecoveryObservation = sqlx::query_as(
        "select recovery_request_id, recovered_at, recovery_attempt_count,
                recovery_retry_delay_ms, recovery_total_latency_ms
           from model_requests where id = 'req_continuation_failed'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load recovered continuation failure");

    assert_eq!(recovered.0.as_deref(), Some("req_continuation_recovery"));
    assert!(recovered.1.is_some());
    assert_eq!(recovered.2, 1);
    assert_eq!(recovered.3, Some(4_000));
    assert!(recovered.4.is_some_and(|latency| latency >= 4_000));
    database.close().await;
}

#[tokio::test]
async fn successful_http_downgrade_should_mark_pending_websocket_failures_recovered() {
    let Some(database) = TestDatabase::create("execution_transport_downgrade_recovered").await
    else {
        return;
    };
    let affinity_hash = "d".repeat(64);
    let first_failure_at = Utc::now() - Duration::seconds(8);
    let second_failure_at = first_failure_at + Duration::seconds(1);
    seed_websocket_transport_failure(
        &database.pool,
        "req_websocket_failed_first",
        &affinity_hash,
        first_failure_at,
    )
    .await;
    seed_websocket_transport_failure(
        &database.pool,
        "req_websocket_failed_second",
        &affinity_hash,
        second_failure_at,
    )
    .await;
    seed_websocket_transport_failure(
        &database.pool,
        "req_websocket_failed_after_delivery",
        &affinity_hash,
        second_failure_at,
    )
    .await;
    sqlx::query(
        "update model_requests
            set downstream_committed_at = completed_at
          where id = 'req_websocket_failed_after_delivery'",
    )
    .execute(&database.pool)
    .await
    .expect("mark control failure delivered downstream");

    let recovery_started_at = first_failure_at + Duration::seconds(4);
    seed_transport_recovery_request(
        &database.pool,
        "req_http_fallback_succeeded",
        &affinity_hash,
        recovery_started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    let mut recovery = successful_core_finalization("req_http_fallback_succeeded");
    recovery.upstream_transport = Some("http_sse".to_owned());
    recovery.downstream_committed_at = Some(std::time::SystemTime::now());
    ExecutionStore::finalize_model_request(&store, recovery)
        .await
        .expect("finalize successful HTTP fallback");

    type PersistedRecovery = (
        String,
        Option<String>,
        Option<chrono::DateTime<Utc>>,
        i32,
        Option<i64>,
        Option<i64>,
    );
    let recovered: Vec<PersistedRecovery> = sqlx::query_as(
        "select id, recovery_request_id, recovered_at, recovery_attempt_count,
                recovery_retry_delay_ms, recovery_total_latency_ms
           from model_requests
          where id like 'req_websocket_failed_%'
          order by id",
    )
    .fetch_all(&database.pool)
    .await
    .expect("load WebSocket recovery observations");

    assert_eq!(recovered.len(), 3);
    let delivered = &recovered[0];
    assert_eq!(delivered.0, "req_websocket_failed_after_delivery");
    assert_eq!((&delivered.1, delivered.2, delivered.3), (&None, None, 0));
    for (row, expected_delay_ms) in recovered[1..].iter().zip([4_000, 3_000]) {
        assert_eq!(row.1.as_deref(), Some("req_http_fallback_succeeded"));
        assert!(row.2.is_some());
        assert_eq!(row.3, 1);
        assert_eq!(row.4, Some(expected_delay_ms));
        assert!(row.5.is_some_and(|latency| latency >= expected_delay_ms));
    }
    let repository = super::observability_repository(&database.pool);
    let failed_detail = repository
        .usage_record_detail("req_websocket_failed_first")
        .await
        .unwrap();
    assert_eq!(
        failed_detail.related_requests[0]["requestId"],
        "req_http_fallback_succeeded"
    );
    assert_eq!(
        failed_detail.related_requests[0]["relation"],
        "recovered_by"
    );
    let recovered_detail = repository
        .usage_record_detail("req_http_fallback_succeeded")
        .await
        .unwrap();
    assert_eq!(recovered_detail.related_requests.len(), 2);
    assert!(
        recovered_detail
            .related_requests
            .iter()
            .all(|related| related["relation"] == "recovers")
    );
    database.close().await;
}

#[tokio::test]
async fn websocket_success_or_late_http_success_should_not_hide_transport_failure() {
    let Some(database) = TestDatabase::create("execution_transport_downgrade_unresolved").await
    else {
        return;
    };
    let failure_at = Utc::now() - Duration::seconds(60);
    let store = PgExecutionStore::new(database.pool.clone());

    let websocket_affinity = "e".repeat(64);
    seed_websocket_transport_failure(
        &database.pool,
        "req_websocket_control_failure",
        &websocket_affinity,
        failure_at,
    )
    .await;
    seed_transport_recovery_request(
        &database.pool,
        "req_websocket_control_success",
        &websocket_affinity,
        failure_at + Duration::seconds(4),
    )
    .await;
    ExecutionStore::finalize_model_request(
        &store,
        successful_core_finalization("req_websocket_control_success"),
    )
    .await
    .expect("finalize successful WebSocket control request");

    let late_affinity = "f".repeat(64);
    seed_websocket_transport_failure(
        &database.pool,
        "req_websocket_late_failure",
        &late_affinity,
        failure_at,
    )
    .await;
    seed_transport_recovery_request(
        &database.pool,
        "req_http_late_success",
        &late_affinity,
        failure_at + Duration::seconds(31),
    )
    .await;
    let mut late_recovery = successful_core_finalization("req_http_late_success");
    late_recovery.upstream_transport = Some("http_sse".to_owned());
    ExecutionStore::finalize_model_request(&store, late_recovery)
        .await
        .expect("finalize late HTTP request");

    type UnresolvedTransportFailure = (String, Option<String>, Option<chrono::DateTime<Utc>>, i32);
    let unresolved: Vec<UnresolvedTransportFailure> = sqlx::query_as(
        "select id, recovery_request_id, recovered_at, recovery_attempt_count
               from model_requests
              where id in ('req_websocket_control_failure', 'req_websocket_late_failure')
              order by id",
    )
    .fetch_all(&database.pool)
    .await
    .expect("load unresolved transport failures");
    assert_eq!(unresolved.len(), 2);
    assert!(
        unresolved
            .iter()
            .all(|row| row.1.is_none() && row.2.is_none() && row.3 == 0)
    );
    database.close().await;
}

#[tokio::test]
async fn failed_or_late_new_chain_should_leave_continuation_failure_unresolved() {
    let Some(database) = TestDatabase::create("execution_continuation_unresolved").await else {
        return;
    };
    let affinity_hash = "c".repeat(64);
    let failure_completed_at = Utc::now() - Duration::seconds(60);
    seed_continuation_failure(
        &database.pool,
        "req_continuation_unresolved",
        &affinity_hash,
        failure_completed_at,
    )
    .await;
    seed_recovery_request(
        &database.pool,
        "req_continuation_retry_failed",
        &affinity_hash,
        failure_completed_at + Duration::seconds(3),
    )
    .await;

    let store = PgExecutionStore::new(database.pool.clone());
    let mut failed = successful_core_finalization("req_continuation_retry_failed");
    failed.outcome = ExecutionOutcome::Failed;
    failed.client_status_code = Some(502);
    failed.upstream_status_code = Some(500);
    failed.error = Some(GatewayError::new(
        GatewayErrorKind::UpstreamUnavailable,
        "recovery request failed",
    ));
    ExecutionStore::finalize_model_request(&store, failed)
        .await
        .expect("finalize failed recovery request");

    let after_failed: (Option<String>, Option<chrono::DateTime<Utc>>, i32) = sqlx::query_as(
        "select recovery_request_id, recovered_at, recovery_attempt_count
           from model_requests where id = 'req_continuation_unresolved'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load unresolved continuation failure");
    assert_eq!(after_failed, (None, None, 1));

    seed_recovery_request(
        &database.pool,
        "req_continuation_retry_late",
        &affinity_hash,
        failure_completed_at + Duration::seconds(31),
    )
    .await;
    ExecutionStore::finalize_model_request(
        &store,
        successful_core_finalization("req_continuation_retry_late"),
    )
    .await
    .expect("finalize late recovery request");

    let after_late: (Option<String>, Option<chrono::DateTime<Utc>>, i32) = sqlx::query_as(
        "select recovery_request_id, recovered_at, recovery_attempt_count
           from model_requests where id = 'req_continuation_unresolved'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load late unresolved continuation failure");
    assert_eq!(after_late, (None, None, 1));
    database.close().await;
}

async fn seed_continuation_failure(
    pool: &sqlx::PgPool,
    id: &str,
    affinity_hash: &str,
    completed_at: chrono::DateTime<Utc>,
) {
    seed_running_request(pool, id)
        .await
        .expect("seed continuation failure request");
    sqlx::query(
        "update model_requests
            set client_api_key_ref = 'key_continuation', client_transport = 'websocket',
                continuation_affinity_hash = $2,
                continuation_previous_response_id_hash = $3,
                continuation_requested = true,
                outcome = 'failed', error_kind = 'continuation_recovery_required',
                continuation_unavailable_reason = 'reused_connection_lost',
                started_at = $4 - interval '1 second', deadline_at = $4,
                completed_at = $4
          where id = $1",
    )
    .bind(id)
    .bind(affinity_hash)
    .bind("b".repeat(64))
    .bind(completed_at)
    .execute(pool)
    .await
    .expect("finalize seeded continuation failure");
}

async fn seed_recovery_request(
    pool: &sqlx::PgPool,
    id: &str,
    affinity_hash: &str,
    started_at: chrono::DateTime<Utc>,
) {
    seed_running_request(pool, id)
        .await
        .expect("seed continuation recovery request");
    sqlx::query(
        "update model_requests
            set client_api_key_ref = 'key_continuation', client_transport = 'websocket',
                continuation_affinity_hash = $2, continuation_requested = false,
                started_at = $3, deadline_at = $3 + interval '1 minute'
          where id = $1",
    )
    .bind(id)
    .bind(affinity_hash)
    .bind(started_at)
    .execute(pool)
    .await
    .expect("prepare seeded recovery request");
}

async fn seed_websocket_transport_failure(
    pool: &sqlx::PgPool,
    id: &str,
    affinity_hash: &str,
    completed_at: chrono::DateTime<Utc>,
) {
    seed_running_request(pool, id)
        .await
        .expect("seed WebSocket transport failure request");
    sqlx::query(
        "update model_requests
            set client_api_key_ref = 'key_transport_recovery',
                client_transport = 'http_sse',
                continuation_affinity_hash = $2,
                attempt_count = 1, upstream_send_state = 'ambiguous',
                upstream_transport = 'websocket', outcome = 'failed',
                error_kind = 'upstream_unavailable',
                provider_error_code = 'websocket_close_1000',
                started_at = $3 - interval '1 second', deadline_at = $3,
                completed_at = $3
          where id = $1",
    )
    .bind(id)
    .bind(affinity_hash)
    .bind(completed_at)
    .execute(pool)
    .await
    .expect("finalize seeded WebSocket transport failure");
}

async fn seed_transport_recovery_request(
    pool: &sqlx::PgPool,
    id: &str,
    affinity_hash: &str,
    started_at: chrono::DateTime<Utc>,
) {
    seed_running_request(pool, id)
        .await
        .expect("seed transport recovery request");
    sqlx::query(
        "update model_requests
            set client_api_key_ref = 'key_transport_recovery',
                client_transport = 'http_sse',
                continuation_affinity_hash = $2,
                started_at = $3, deadline_at = $3 + interval '1 minute'
          where id = $1",
    )
    .bind(id)
    .bind(affinity_hash)
    .bind(started_at)
    .execute(pool)
    .await
    .expect("prepare seeded transport recovery request");
}

async fn seed_running_request(pool: &sqlx::PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, provider_kind, provider_account_ref, cost_source,
           started_at, deadline_at,
           routing_scope, routing_group_refs, routing_group_names_snapshot
         ) values ($1, 'key_status', 1, 'openai_responses', 'generate', '/v1/responses',
           'http_json', 'status-model', 'openai', 'acct_status', 'unavailable', now(), now() + interval '1 minute',
           'all', '{}'::text[], '[]'::jsonb)",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn diagnostic_trace_is_finalized_atomically_and_available_for_failed_request_details() {
    let Some(database) = TestDatabase::create("diagnostic_trace").await else {
        return;
    };
    seed_running_request(&database.pool, "req_diagnostic_failed")
        .await
        .unwrap();
    let store = PgExecutionStore::new(database.pool.clone());
    let mut finalization = successful_core_finalization("req_diagnostic_failed");
    finalization.outcome = ExecutionOutcome::Failed;
    finalization.client_status_code = Some(502);
    let diagnostic = gateway_core::error::ProviderDiagnostic::new(
        "OpenAI WebSocket receive idle timeout after 300s",
    )
    .with_classification("receive", "receive_idle_timeout");
    let provider_error = gateway_core::error::ProviderError::new(
        gateway_core::error::ProviderErrorKind::Timeout,
        UpstreamSendState::Ambiguous,
    )
    .with_diagnostic(diagnostic);
    finalization.error = Some(GatewayError::from_provider(&provider_error));
    let trace = serde_json::json!({"schemaVersion": 1, "requestId": "req_diagnostic_failed", "events": [
        {"attemptIndex": 1, "stage": "attempt.failed", "data": {"diagnostic": {
            "stage": "receive", "code": "receive_idle_timeout", "message": "OpenAI WebSocket receive idle timeout after 300s"
        }}},
    ]});
    finalization.diagnostic_trace_json = Some(trace.to_string());
    ExecutionStore::finalize_model_request(&store, finalization)
        .await
        .unwrap();
    let persisted: (String, serde_json::Value, String) = sqlx::query_as(
        "select outcome, diagnostic_trace_json, error_message from model_requests where id = 'req_diagnostic_failed'"
    ).fetch_one(&database.pool).await.unwrap();
    assert_eq!(
        persisted,
        (
            "failed".to_owned(),
            trace.clone(),
            "OpenAI WebSocket receive idle timeout after 300s".to_owned()
        )
    );
    let repository = super::observability_repository(&database.pool);
    let detail = repository
        .usage_record_detail("req_diagnostic_failed")
        .await
        .unwrap();
    assert_eq!(detail.trace, Some(trace));
    assert!(detail.related_requests.is_empty());
    database.close().await;
}

// 只构造已接纳的入口事实；未选择账号、未出站，也没有上游用量。
pub(super) fn accepted_request(id: &str) -> CoreNewModelRequest {
    let started_at = SystemTime::from(
        DateTime::from_timestamp_micros(Utc::now().timestamp_micros())
            .expect("PostgreSQL timestamp precision"),
    );
    CoreNewModelRequest {
        id: ModelRequestId::new(id).expect("request id"),
        client_api_key_id: None,
        client_api_key_ref: ClientApiKeyId::new("key_zero_attempt").expect("client key ref"),
        config_revision: ConfigRevision::new(1).expect("revision"),
        routing: AccountRoutingSnapshot::all(),
        protocol: "openai".to_owned(),
        operation: OperationKind::Generate,
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model: Some(PublicModelId::new("coding").expect("public model")),
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        request_kind: None,
        subagent_kind: None,
        compact: false,
        continuation: Default::default(),
        image_generation_requested: false,
        admission_decision_ms: Some(2),
        started_at,
        deadline_at: started_at + StdDuration::from_secs(30),
    }
}

pub(super) fn early_failure(request: &CoreNewModelRequest) -> CoreModelRequestFinalization {
    let trace = TraceContext::new(request.id.as_str());
    trace.record("request.started", json!({"operation": "generate"}));
    // trace 的 index 是预备阶段关联，不证明已经拿到账号、建流或实际发送。
    let preparation = trace.attempt(1);
    preparation.record(
        "attempt.started",
        json!({"provider": "openai", "model": "coding", "pinnedAccount": null}),
    );
    preparation.record(
        "attempt.failed",
        json!({"kind": "no_eligible_account", "sendState": "NotSent"}),
    );
    trace.record(
        "request.finished",
        json!({"outcome": "failed", "errorKind": "no_available_provider", "attemptCount": 0}),
    );
    CoreModelRequestFinalization {
        request_id: request.id.clone(),
        outcome: ExecutionOutcome::Failed,
        send_state: UpstreamSendState::NotSent,
        attempt_count: 0,
        downstream_committed_at: None,
        client_status_code: Some(503),
        client_response_id: None,
        upstream_status_code: None,
        upstream_request_id: None,
        upstream_response_id: None,
        upstream_transport: None,
        http_version: None,
        websocket_pool: None,
        service_tier: None,
        provider_metadata_json: None,
        diagnostic_trace_json: trace.snapshot().map(|value| value.to_string()),
        error: Some(GatewayError::new(
            GatewayErrorKind::NoAvailableProvider,
            "no available provider",
        )),
        provider_error_code: None,
        raw_upstream_error: None,
        failure_observation: Default::default(),
        retry_after_ms: None,
        usage: Usage::new(),
        image_generation_succeeded: None,
        cost: CostEstimate::unavailable(),
        timings: CoreModelRequestTimings {
            latency_ms: Some(1_000),
            ..Default::default()
        },
        completed_at: request.started_at + StdDuration::from_secs(1),
    }
}

async fn stored_row(pool: &PgPool, request_id: &str) -> Value {
    sqlx::query_scalar("select to_jsonb(mr) from model_requests mr where id = $1")
        .bind(request_id)
        .fetch_one(pool)
        .await
        .expect("load complete synthetic request row")
}

fn zero_attempt_range(request: &CoreNewModelRequest) -> ObservabilityRange {
    ObservabilityRange::new(
        DateTime::from(request.started_at - StdDuration::from_secs(60)),
        DateTime::from(request.deadline_at + StdDuration::from_secs(60)),
    )
    .expect("observation range")
}

#[tokio::test]
async fn zero_attempt_failure_is_queryable_without_fabricating_upstream_facts() {
    let Some(database) = TestDatabase::create("zero_attempt_queries").await else {
        return;
    };
    let store = PgExecutionStore::new(database.pool.clone());
    let request = accepted_request("req_zero_attempt_queries");
    store
        .create_model_request(request.clone())
        .await
        .expect("create accepted request without attempt");
    let finalization = early_failure(&request);
    let trace: Value = serde_json::from_str(
        finalization
            .diagnostic_trace_json
            .as_deref()
            .expect("trace snapshot"),
    )
    .expect("trace JSON");
    ExecutionStore::finalize_model_request(&store, finalization)
        .await
        .expect("finalize zero-attempt failure with trace");

    let repository = observability_repository(&database.pool);
    let detail = repository
        .usage_record_detail(request.id.as_str())
        .await
        .expect("load failed request detail");
    assert_eq!(detail.request.outcome, "failed");
    assert_eq!(detail.request.attempt_count, 0);
    assert_eq!(detail.request.upstream_send_state, "not_sent");
    assert_eq!(detail.request.client_api_key_ref, "key_zero_attempt");
    assert_eq!(detail.request.requested_model_id.as_deref(), Some("coding"));
    assert_eq!(detail.request.admission_decision_ms, Some(2));
    assert_eq!(detail.request.latency_ms, Some(1_000));
    assert_eq!(detail.request.client_status_code, Some(503));
    assert_eq!(
        detail.request.error_kind.as_deref(),
        Some("no_available_provider")
    );
    assert_eq!(
        detail.request.error_message.as_deref(),
        Some("no available provider")
    );
    assert!(detail.attempts.is_empty());
    assert!(detail.related_requests.is_empty());
    assert_eq!(detail.trace, Some(trace.clone()));
    let row = stored_row(&database.pool, request.id.as_str()).await;
    for field in [
        "provider_kind",
        "provider_account_id",
        "provider_account_ref",
        "provider_account_name_snapshot",
        "provider_account_email_snapshot",
        "provider_account_authentication_kind_snapshot",
        "upstream_model_id",
        "upstream_transport",
        "http_version",
        "upstream_status_code",
        "upstream_request_id",
        "upstream_response_id",
        "client_response_id",
        "downstream_committed_at",
        "service_tier",
        "provider_observation_json",
        "raw_upstream_error",
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "cost_amount",
        "cost_currency",
        "account_selection_wait_ms",
        "capacity_used_slots",
        "capacity_total_slots",
    ] {
        assert_eq!(
            row.get(field),
            Some(&Value::Null),
            "{field} must remain absent"
        );
    }
    assert_eq!(row["cost_source"], "unavailable");
    let events = trace["events"].as_array().expect("events");
    assert!(events.iter().all(|event| event["exchangeId"].is_null()));
    let prepared_attempts: Vec<_> = events
        .iter()
        .filter(|event| event["attemptIndex"] == 1)
        .map(|event| event["stage"].as_str().expect("stage"))
        .collect();
    assert_eq!(prepared_attempts, ["attempt.started", "attempt.failed"]);

    let query = OpsErrorQuery {
        range: zero_attempt_range(&request),
        filter: OpsErrorFilter {
            request_id: Some(request.id.as_str().to_owned()),
            ..Default::default()
        },
        current_page: 1,
        page_size: ObservabilityPageSize::new(10).expect("page size"),
    };
    let errors = repository
        .list_ops_errors(query.clone())
        .await
        .expect("ops errors");
    assert_eq!(errors.total, 1);
    assert_eq!(errors.items.len(), 1);
    let error = &errors.items[0];
    assert_eq!(error.source, "model_request");
    assert_eq!(error.request_id.as_deref(), Some(request.id.as_str()));
    assert_eq!(error.attempt_index, None);
    assert_eq!(error.failure_kind, "no_available_provider");
    assert_eq!(error.message, "no available provider");
    assert_eq!(error.upstream_send_state.as_deref(), Some("not_sent"));
    assert_eq!(error.provider_kind, None);
    assert_eq!(error.provider_account_ref, None);
    assert_eq!(error.upstream_transport, None);
    assert_eq!(error.upstream_status_code, None);
    assert_eq!(error.upstream_request_id, None);
    assert_eq!(error.raw_upstream_error, None);

    // 路由入口不等于已选择 Provider；没有平台事实时不能被平台/attempt 筛选命中。
    for filter in [
        OpsErrorFilter {
            provider_kind: Some("openai".to_owned()),
            ..query.filter.clone()
        },
        OpsErrorFilter {
            attempt_index: Some(1),
            ..query.filter.clone()
        },
    ] {
        let page = repository
            .list_ops_errors(OpsErrorQuery {
                filter,
                ..query.clone()
            })
            .await
            .expect("filter absent upstream facts");
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
    }
    let admin = admin_observability_store(&database.pool);
    let admin_detail = admin
        .usage_record_detail(request.id.as_str())
        .await
        .expect("admin detail");
    assert_eq!(admin_detail.trace, Some(trace));
    assert_eq!(admin_detail.request.attempt_count, 0);
    assert!(admin_detail.attempts.is_empty());
    let admin_errors = admin
        .list_ops_errors(admin_observability::OpsErrorQuery {
            range: admin_observability::TimeRange::new(query.range.start, query.range.end)
                .expect("admin time range"),
            filter: admin_observability::OpsErrorFilter {
                request_id: Some(request.id.as_str().to_owned()),
                ..Default::default()
            },
            current_page: 1,
            page_size: PageSize::new(10).expect("page size"),
        })
        .await
        .expect("admin ops errors without provider facts");
    assert_eq!(admin_errors.total, 1);
    assert_eq!(admin_errors.items.len(), 1);
    assert_eq!(admin_errors.items[0].attempt_index, None);
    assert_eq!(admin_errors.items[0].provider_kind, None);
    database.close().await;
}

#[tokio::test]
async fn zero_attempt_finalization_rejects_sent_or_missing_attempt_facts_atomically() {
    let Some(database) = TestDatabase::create("zero_attempt_constraints").await else {
        return;
    };
    let store = PgExecutionStore::new(database.pool.clone());
    let request = accepted_request("req_zero_attempt_constraints");
    store
        .create_model_request(request.clone())
        .await
        .expect("create request");
    let before = stored_row(&database.pool, request.id.as_str()).await;

    for (count, send_state) in [
        (0, UpstreamSendState::Sent),
        (0, UpstreamSendState::Ambiguous),
        (1, UpstreamSendState::NotSent),
    ] {
        let mut finalization = early_failure(&request);
        finalization.attempt_count = count;
        finalization.send_state = send_state;
        assert!(
            ExecutionStore::finalize_model_request(&store, finalization)
                .await
                .is_err()
        );
        assert_eq!(
            stored_row(&database.pool, request.id.as_str()).await,
            before
        );
    }
    for invalid_trace in [
        "not JSON".to_owned(),
        "[]".to_owned(),
        json!({"data": "x".repeat(65_536)}).to_string(),
    ] {
        let mut finalization = early_failure(&request);
        finalization.diagnostic_trace_json = Some(invalid_trace);
        assert_eq!(
            ExecutionStore::finalize_model_request(&store, finalization)
                .await
                .expect_err("invalid trace")
                .kind(),
            StoreErrorKind::InvalidData,
        );
        assert_eq!(
            stored_row(&database.pool, request.id.as_str()).await,
            before
        );
    }
    ExecutionStore::finalize_model_request(&store, early_failure(&request))
        .await
        .expect("valid zero attempt");
    database.close().await;
}

#[tokio::test]
async fn zero_attempt_duplicate_create_and_finalization_never_overwrite_terminal_facts() {
    let Some(database) = TestDatabase::create("zero_attempt_idempotency").await else {
        return;
    };
    let store = PgExecutionStore::new(database.pool.clone());
    let request = accepted_request("req_zero_attempt_idempotency");
    store
        .create_model_request(request.clone())
        .await
        .expect("create request");
    let (first, second) = tokio::join!(
        ExecutionStore::finalize_model_request(&store, early_failure(&request)),
        ExecutionStore::finalize_model_request(&store, early_failure(&request)),
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        first
            .err()
            .or_else(|| second.err())
            .expect("CAS loser")
            .kind(),
        StoreErrorKind::InvalidState
    );
    let terminal = stored_row(&database.pool, request.id.as_str()).await;
    assert!(store.create_model_request(request.clone()).await.is_err());
    let mut late = early_failure(&request);
    late.outcome = ExecutionOutcome::Cancelled;
    late.error = Some(GatewayError::new(
        GatewayErrorKind::Cancelled,
        "request was cancelled",
    ));
    late.diagnostic_trace_json = None;
    late.completed_at += StdDuration::from_secs(1);
    assert_eq!(
        ExecutionStore::finalize_model_request(&store, late)
            .await
            .expect_err("already finalized")
            .kind(),
        StoreErrorKind::InvalidState
    );
    assert_eq!(
        store
            .recover_expired(request.deadline_at)
            .await
            .expect("recover")
            .requests,
        0
    );
    assert_eq!(
        stored_row(&database.pool, request.id.as_str()).await,
        terminal
    );
    let count: i64 = sqlx::query_scalar("select count(*) from model_requests")
        .fetch_one(&database.pool)
        .await
        .expect("one request row");
    assert_eq!(count, 1);
    let ops: i64 = sqlx::query_scalar("select count(*) from ops_events")
        .fetch_one(&database.pool)
        .await
        .expect("no extra error ledger");
    assert_eq!(ops, 0);
    database.close().await;
}

#[tokio::test]
async fn zero_attempt_recovery_respects_deadline_and_does_not_invent_a_trace() {
    let Some(database) = TestDatabase::create("zero_attempt_recovery").await else {
        return;
    };
    let store = PgExecutionStore::new(database.pool.clone());
    let request = accepted_request("req_zero_attempt_recovery");
    let mut active = request.clone();
    active.id = ModelRequestId::new("req_zero_attempt_still_running").expect("request id");
    active.deadline_at += StdDuration::from_secs(30);
    store
        .create_model_request(request.clone())
        .await
        .expect("create interrupted request");
    store
        .create_model_request(active.clone())
        .await
        .expect("create active request");
    assert_eq!(
        store
            .recover_expired(request.deadline_at - StdDuration::from_micros(1))
            .await
            .expect("before deadline")
            .requests,
        0
    );
    assert_eq!(
        store
            .recover_expired(request.deadline_at)
            .await
            .expect("at deadline")
            .requests,
        1
    );
    let recovered = stored_row(&database.pool, request.id.as_str()).await;
    assert_eq!(
        store
            .recover_expired(request.deadline_at)
            .await
            .expect("repeat recovery")
            .requests,
        0
    );
    assert_eq!(
        ExecutionStore::finalize_model_request(&store, early_failure(&request))
            .await
            .expect_err("late finalize after recovery")
            .kind(),
        StoreErrorKind::InvalidState
    );
    assert_eq!(
        stored_row(&database.pool, request.id.as_str()).await,
        recovered
    );
    let repository = observability_repository(&database.pool);
    let detail = repository
        .usage_record_detail(request.id.as_str())
        .await
        .expect("recovered detail");
    assert_eq!(detail.request.outcome, "incomplete");
    assert_eq!(
        detail.request.error_kind.as_deref(),
        Some("process_interrupted")
    );
    assert_eq!(
        detail.request.completed_at,
        Some(DateTime::from(request.deadline_at))
    );
    assert_eq!(detail.request.attempt_count, 0);
    assert_eq!(detail.request.upstream_send_state, "not_sent");
    assert_eq!(detail.trace, None);
    assert!(detail.attempts.is_empty());
    let errors = repository
        .list_ops_errors(OpsErrorQuery {
            range: zero_attempt_range(&request),
            filter: OpsErrorFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("recovered ops error");
    assert_eq!(errors.total, 1);
    assert_eq!(errors.items[0].failure_kind, "process_interrupted");
    assert_eq!(errors.items[0].attempt_index, None);
    assert_eq!(
        repository
            .usage_record_detail(active.id.as_str())
            .await
            .expect("active detail")
            .request
            .outcome,
        "running"
    );
    database.close().await;
}

#[tokio::test]
async fn zero_attempt_failure_does_not_enter_successful_usage_or_cost_aggregates() {
    let Some(database) = TestDatabase::create("zero_attempt_metrics").await else {
        return;
    };
    let store = PgExecutionStore::new(database.pool.clone());
    let request = accepted_request("req_zero_attempt_metrics");
    seed_running_request(&database.pool, "req_zero_attempt_success_control")
        .await
        .expect("success control");
    let mut success = successful_core_finalization("req_zero_attempt_success_control");
    success.downstream_committed_at = Some(success.completed_at);
    success.usage = Usage {
        input_tokens: Some(100),
        output_tokens: Some(20),
        total_tokens: Some(120),
        ..Usage::new()
    };
    success.timings.latency_ms = Some(200);
    success.cost = gateway_core::metering::CalculatedCost::from_usd_ticks(12_500_000_000)
        .expect("calculated cost")
        .into_estimate();
    ExecutionStore::finalize_model_request(&store, success)
        .await
        .expect("finalize success control");
    let repository = observability_repository(&database.pool);
    let before = repository
        .usage_summary(zero_attempt_range(&request), UsageRecordFilter::default())
        .await
        .expect("baseline usage");
    assert_eq!(before.requests.total_tokens, 120);
    assert!(!before.attempts.costs.is_empty());
    store
        .create_model_request(request.clone())
        .await
        .expect("create zero attempt");
    ExecutionStore::finalize_model_request(&store, early_failure(&request))
        .await
        .expect("finalize failure");
    let after = repository
        .usage_summary(zero_attempt_range(&request), UsageRecordFilter::default())
        .await
        .expect("usage after failure");
    assert_eq!(
        after.requests.request_count,
        before.requests.request_count + 1
    );
    assert_eq!(
        after.requests.failure_count,
        before.requests.failure_count + 1
    );
    assert_eq!(after.requests.success_count, before.requests.success_count);
    assert_eq!(after.requests.total_tokens, before.requests.total_tokens);
    assert_eq!(after.requests.latency_sum, before.requests.latency_sum);
    assert_eq!(after.requests.latency_count, before.requests.latency_count);
    assert_eq!(after.attempts, before.attempts);
    let successes = repository
        .usage_summary(
            zero_attempt_range(&request),
            UsageRecordFilter {
                outcome: Some("succeeded".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("successful aggregation unchanged");
    assert_eq!(successes, before);
    let filter = UsageRecordFilter {
        request_id: Some(request.id.as_str().to_owned()),
        ..Default::default()
    };
    let failed = repository
        .usage_summary(zero_attempt_range(&request), filter.clone())
        .await
        .expect("failed request metrics");
    assert_eq!(failed.requests.request_count, 1);
    assert_eq!(failed.requests.failure_count, 1);
    assert_eq!(failed.requests.total_tokens, 0);
    assert_eq!(failed.requests.latency_count, 0);
    assert_eq!(failed.attempts, AttemptMetrics::default());
    let page = repository
        .list_usage_records(UsageRecordQuery {
            range: zero_attempt_range(&request),
            filter: filter.clone(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("successful usage list excludes failure");
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
    let billing: Vec<_> = repository
        .usage_calculated_billing_facts(zero_attempt_range(&request), filter)
        .try_collect()
        .await
        .expect("billing facts");
    assert!(billing.is_empty());
    let charges: i64 = sqlx::query_scalar("select count(*) from client_key_charge_events")
        .fetch_one(&database.pool)
        .await
        .expect("observations do not settle budgets");
    assert_eq!(charges, 0);
    database.close().await;
}
