use chrono::Utc;
use gateway_store::postgres::{
    ModelRequestHistoryRecord, ModelRequestHistoryRepository, PgHistoryRepository,
};

use super::TestDatabase;

#[test]
fn history_contains_no_payload_field() {
    let history = ModelRequestHistoryRecord {
        id: "request-1".to_owned(),
        client_api_key_ref: "key-1".to_owned(),
        requested_model_id: "coding".to_owned(),
        provider_kind: None,
        provider_account_ref: None,
        upstream_model_id: None,
        outcome: "failed".to_owned(),
        client_response_id: None,
        upstream_response_id: None,
        started_at: Utc::now(),
        completed_at: None,
    };
    assert_eq!(history.id, "request-1");
}

#[tokio::test]
async fn native_pin_is_caller_scoped_and_uses_live_account_and_upstream_handle() {
    let Some(database) = TestDatabase::create("native_pin").await else {
        return;
    };
    seed_native_history(&database.pool)
        .await
        .expect("seed native history");
    let repository = PgHistoryRepository::new(database.pool.clone());

    let pin = repository
        .resolve_native_continuation_pin("resp_gateway_visible", "key_owner")
        .await
        .expect("resolve native pin")
        .expect("eligible pin");
    assert_eq!(pin.previous_response_id().as_str(), "resp_gateway_visible");
    assert_eq!(pin.upstream_response_id().as_str(), "resp_upstream_native");
    assert_eq!(pin.account().as_str(), "acct_history");
    let debug = format!("{pin:?}");
    assert!(!debug.contains("resp_gateway_visible"));
    assert!(!debug.contains("resp_upstream_native"));

    let other_client = repository
        .resolve_native_continuation_pin("resp_gateway_visible", "key_other")
        .await
        .expect("resolve other caller");
    assert!(other_client.is_none());

    sqlx::query("delete from provider_accounts where id = 'acct_history'")
        .execute(&database.pool)
        .await
        .expect("delete live account while preserving historical ref");
    let deleted_account = repository
        .resolve_native_continuation_pin("resp_gateway_visible", "key_owner")
        .await
        .expect("resolve deleted account");
    assert!(deleted_account.is_none());
    let historical_ref: Option<String> = sqlx::query_scalar(
        "select provider_account_ref from model_requests where id = 'req_history'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load historical account ref");
    assert_eq!(historical_ref.as_deref(), Some("acct_history"));

    database.close().await;
}

#[tokio::test]
async fn native_pin_requires_committed_success_but_not_a_specific_transport() {
    let Some(database) = TestDatabase::create("native_pin_boundaries").await else {
        return;
    };
    seed_native_history(&database.pool)
        .await
        .expect("seed native history");
    let repository = PgHistoryRepository::new(database.pool.clone());

    sqlx::query(
        "update model_requests set downstream_committed_at = null where id = 'req_history'",
    )
    .execute(&database.pool)
    .await
    .expect("clear downstream commit");
    assert!(
        repository
            .resolve_native_continuation_pin("resp_gateway_visible", "key_owner",)
            .await
            .expect("resolve uncommitted request")
            .is_none()
    );

    sqlx::query(
        "update model_requests
         set downstream_committed_at = now(), outcome = 'failed'
         where id = 'req_history'",
    )
    .execute(&database.pool)
    .await
    .expect("mark request failed");
    assert!(
        repository
            .resolve_native_continuation_pin("resp_gateway_visible", "key_owner",)
            .await
            .expect("resolve failed request")
            .is_none()
    );

    sqlx::query("update model_requests set outcome = 'succeeded' where id = 'req_history'")
        .execute(&database.pool)
        .await
        .expect("restore successful request");
    assert!(
        repository
            .resolve_native_continuation_pin("resp_gateway_visible", "key_owner",)
            .await
            .expect("resolve restored request")
            .is_some()
    );

    sqlx::query(
        "update model_requests set upstream_transport = 'http_sse' where id = 'req_history'",
    )
    .execute(&database.pool)
    .await
    .expect("mark response connection-independent");
    assert!(
        repository
            .resolve_native_continuation_pin("resp_gateway_visible", "key_owner",)
            .await
            .expect("resolve HTTP response")
            .is_some()
    );

    database.close().await;
}

async fn seed_native_history(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, upstream_user_id,
           provider_credentials_json, credential_revision, has_refresh_token,
           access_token_expires_at, enabled, availability, availability_observed_at,
           created_at, updated_at
         ) values (
           'acct_history', 'openai', 'history', 'user-history',
           '{}'::jsonb, 1, false, now() + interval '1 day', true, 'ready',
           now(), now(), now()
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, input_token_estimate,
           provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           upstream_send_state, downstream_committed_at, outcome, client_status_code,
           upstream_status_code, client_response_id, upstream_response_id,
           cost_source, started_at, deadline_at, completed_at
         ) values (
           'req_history', 'key_owner', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'history-model', 1, 'openai', 'acct_history',
           'acct_history', 'history-upstream',
           'websocket', 1, 'sent', now(), 'succeeded', 200, 200,
           'resp_gateway_visible', 'resp_upstream_native', 'unavailable',
           now() - interval '1 minute', now() + interval '1 minute', now()
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}
