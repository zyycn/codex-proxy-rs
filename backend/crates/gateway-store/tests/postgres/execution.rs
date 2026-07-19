use chrono::{Duration, Utc};
use gateway_core::accounting::{CalculatedCost, Usage};
use gateway_core::engine::{
    ExecutionOutcome, ExecutionStore, ModelRequestFinalization as CoreModelRequestFinalization,
    ModelRequestId, ModelRequestTimings as CoreModelRequestTimings, UpstreamSendState,
};
use gateway_store::postgres::{ModelRequestRepository, NewModelRequest, PgExecutionStore};

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
        id: "request-1".to_owned(),
        client_api_key_id: Some("key-live".to_owned()),
        client_api_key_ref: "key-history".to_owned(),
        config_revision: 1,
        protocol: "openai".to_owned(),
        operation: "responses".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: "coding".to_owned(),
        input_token_estimate: 0,
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        request_kind: None,
        subagent_kind: None,
        compact: false,
        started_at,
        deadline_at: started_at + Duration::seconds(30),
    };
    assert!(request.validate().is_err());
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

    ExecutionStore::finalize_model_request(
        &repository,
        CoreModelRequestFinalization {
            request_id: ModelRequestId::new("req_calculated_cost").expect("request id"),
            outcome: ExecutionOutcome::Succeeded,
            send_state: UpstreamSendState::Sent,
            attempt_count: 1,
            downstream_committed_at: None,
            client_status_code: Some(200),
            client_response_id: None,
            upstream_status_code: Some(200),
            upstream_request_id: None,
            upstream_response_id: None,
            error: None,
            provider_error_code: None,
            retry_after_ms: None,
            usage: Usage::new(),
            cost: CalculatedCost::from_usd_ticks(12_345)
                .expect("calculated cost")
                .into_estimate(),
            timings: CoreModelRequestTimings::default(),
            completed_at: std::time::SystemTime::now(),
        },
    )
    .await
    .expect("persist calculated cost");
    let persisted: (String, String, String) = sqlx::query_as(
        "select cost_source, cost_amount::text, cost_currency
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
        )
    );
    database.close().await;
}

async fn seed_running_request(pool: &sqlx::PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, input_token_estimate, cost_source,
           started_at, deadline_at
         ) values ($1, 'key_status', 1, 'openai_responses', 'generate', '/v1/responses',
           'http_json', 'status-model', 0, 'unavailable', now(), now() + interval '1 minute')",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
