use chrono::{Duration, Utc};
use gateway_core::engine::{
    ExecutionOutcome, ExecutionStore, ModelRequestFinalization as CoreModelRequestFinalization,
    ModelRequestId, ModelRequestTimings as CoreModelRequestTimings,
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::metering::{CalculatedCost, CostEstimate, Usage};
use gateway_core::upstream::UpstreamSendState;
use gateway_store::postgres::{
    ModelRequestAttemptStart, ModelRequestRepository, NewModelRequest, PgExecutionStore,
};

use super::TestDatabase;

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
