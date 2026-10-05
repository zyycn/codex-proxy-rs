//! 验证数据库拒绝不完整请求事实、缺失快照与无效时间关系

use super::TestDatabase;

async fn seed_request(pool: &sqlx::PgPool) {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, started_at, deadline_at, outcome, completed_at, routing_scope
         ) values (
           'req_integrity', 'deleted_key', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', now(), now() + interval '1 hour', 'failed', now(), 'all'
         )",
    )
    .execute(pool)
    .await
    .expect("seed request without optional facts");
}

fn assert_check_rejected(error: &sqlx::Error) {
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23514")
    );
}

#[tokio::test]
async fn request_fact_groups_reject_partial_writes_and_accept_complete_observations() {
    let Some(db) = TestDatabase::create("fact_groups").await else {
        return;
    };
    seed_request(&db.pool).await;
    for assignments in [
        "cost_source = 'calculated', cost_amount = 1",
        "capacity_used_slots = 1",
        "capacity_total_slots = 2",
        "upstream_connection_id = 'connection'",
        "upstream_connection_id = 'connection', upstream_connection_exit_reason = 'peer_closed', upstream_connection_age_ms = 10",
        "recovery_request_id = 'req_recovery', recovered_at = completed_at, recovery_attempt_count = 1",
        "recovery_request_id = 'req_recovery', recovered_at = completed_at, recovery_attempt_count = 1, recovery_retry_delay_ms = 0",
        "outcome = 'running', completed_at = null, recovered_at = now(), recovery_request_id = 'req_recovery', recovery_attempt_count = 1, recovery_retry_delay_ms = 0, recovery_total_latency_ms = 1",
    ] {
        let error = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update model_requests set {assignments} where id = 'req_integrity'"
        )))
        .execute(&db.pool)
        .await
        .expect_err(assignments);
        assert_check_rejected(&error);
    }
    sqlx::query(
        "update model_requests set cost_source = 'calculated', cost_amount = 1,
           cost_currency = 'USD', capacity_used_slots = 1, capacity_total_slots = 2,
           upstream_connection_id = 'connection', upstream_connection_exit_reason = 'peer_closed',
           upstream_connection_age_ms = 10, upstream_connection_idle_ms = 2,
           recovery_request_id = 'req_recovery', recovered_at = completed_at,
           recovery_attempt_count = 1, recovery_retry_delay_ms = 0, recovery_total_latency_ms = 1
         where id = 'req_integrity'",
    )
    .execute(&db.pool)
    .await
    .expect("complete observations satisfy presence and value constraints");
    db.close().await;
}

#[tokio::test]
async fn account_snapshots_are_required_before_live_foreign_keys_can_be_cleared() {
    let Some(db) = TestDatabase::create("account_fact_refs").await else {
        return;
    };
    seed_request(&db.pool).await;
    sqlx::query(
        "insert into provider_accounts (id, provider_kind, name, authentication_kind,
           provider_credentials_json, has_refresh_token, credential_observed_at, created_at, updated_at)
         values ('acct_integrity', 'openai', 'test', 'oauth', '{}', false, now(), now(), now())",
    ).execute(&db.pool).await.expect("seed account");
    let error = sqlx::query("update model_requests set provider_account_id = 'acct_integrity'")
        .execute(&db.pool)
        .await
        .expect_err("request requires snapshot ref");
    assert_check_rejected(&error);
    let error = sqlx::query(
        "insert into ops_events (id, level, component, operation, provider_account_id,
           failure_kind, message, created_at)
         values ('ops_missing_ref', 'error', 'probe', 'probe', 'acct_integrity', 'timeout', 'test', now())",
    ).execute(&db.pool).await.expect_err("event requires snapshot ref");
    assert_check_rejected(&error);
    sqlx::query(
        "update model_requests set provider_account_id = 'acct_integrity', provider_account_ref = 'acct_integrity'",
    ).execute(&db.pool).await.expect("write complete account reference");
    sqlx::query("delete from provider_accounts where id = 'acct_integrity'")
        .execute(&db.pool)
        .await
        .expect("delete live account through narrowed FK index");
    let refs: (Option<String>, String) = sqlx::query_as(
        "select provider_account_id, provider_account_ref from model_requests where id = 'req_integrity'",
    ).fetch_one(&db.pool).await.expect("read retained snapshot");
    assert_eq!(refs, (None, "acct_integrity".to_owned()));
    db.close().await;
}

#[tokio::test]
async fn audit_requires_canonical_admin_identity_and_retains_it_after_admin_deletion() {
    let Some(db) = TestDatabase::create("audit_identity").await else {
        return;
    };
    sqlx::raw_sql(
        "insert into admin_users (id, password_hash, created_at, updated_at)
           values ('admin_test', 'test_hash', now(), now());
         insert into admin_audit_events (id, actor_kind, actor_admin_user_id, actor_ref,
           action, entity_kind, entity_ref, created_at)
           values ('audit_identity', 'admin_session', 'admin_test', 'admin:admin_test',
             'update', 'settings', '1', now());",
    )
    .execute(&db.pool)
    .await
    .expect("seed audit event with canonical admin identity");
    let error = sqlx::query("update admin_audit_events set actor_ref = 'admin_test'")
        .execute(&db.pool)
        .await
        .expect_err("new writes require the canonical actor identity");
    assert_check_rejected(&error);
    sqlx::query("delete from admin_users where id = 'admin_test'")
        .execute(&db.pool)
        .await
        .expect("audit retains identity when live admin is deleted");
    let identity: (Option<String>, String) =
        sqlx::query_as("select actor_admin_user_id, actor_ref from admin_audit_events")
            .fetch_one(&db.pool)
            .await
            .expect("read historical identity");
    assert_eq!(identity, (None, "admin:admin_test".to_owned()));
    db.close().await;
}

#[tokio::test]
async fn backup_completion_cannot_precede_its_start() {
    let Some(db) = TestDatabase::create("backup_time_order").await else {
        return;
    };
    let error = sqlx::query(
        "insert into backup_records (id, trigger_kind, status, object_key, size_bytes, sha256,
           started_at, completed_at, created_at, updated_at)
         values ('backup_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'manual', 'completed', 'test.dump',
           1, repeat('a', 64), now() + interval '10 seconds', now() + interval '5 seconds',
           now(), now() + interval '20 seconds')",
    )
    .execute(&db.pool)
    .await
    .expect_err("completed backup must not end before it starts");
    assert_check_rejected(&error);
    db.close().await;
}
