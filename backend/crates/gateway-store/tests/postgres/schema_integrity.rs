//! 验证数据库拒绝不完整请求事实、缺失快照与无效时间关系

use super::TestDatabase;

#[tokio::test]
async fn error_details_migration_preserves_existing_request_and_event_text() {
    let Some(db) = TestDatabase::create_through("error_details_upgrade", 22).await else {
        return;
    };
    seed_legacy_request(&db.pool).await;
    let raw = "{ \"error\": {\"code\":\"Vendor.Unknown\",\"message\":\"原始错误\"} }";
    sqlx::query("update model_requests set raw_upstream_error = $1 where id = 'req_integrity'")
        .bind(raw)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into ops_events (id, level, component, operation, failure_kind, message, created_at, raw_upstream_error)
         values ('error_details_upgrade', 'error', 'provider', 'generate', 'unavailable', 'safe message', now(), $1)",
    ).bind(raw).execute(&db.pool).await.unwrap();

    super::TEST_MIGRATOR.run(&db.pool).await.unwrap();
    let details: Vec<String> = sqlx::query_scalar(
        "select error_details from model_requests where id = 'req_integrity'
         union all select error_details from ops_events where id = 'error_details_upgrade'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(details, vec![raw.to_owned(), raw.to_owned()]);
    let old_columns: i64 = sqlx::query_scalar(
        "select count(*) from information_schema.columns
         where table_schema = current_schema() and table_name in ('model_requests', 'ops_events')
         and column_name = 'raw_upstream_error'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(old_columns, 0);
    db.close().await;
}

async fn seed_legacy_request(pool: &sqlx::PgPool) {
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

async fn seed_request(pool: &sqlx::PgPool) {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, started_at, recovery_lease_expires_at, outcome, completed_at, request_observation_json
         ) values (
           'req_integrity', 'deleted_key', 'responses', 'http_sse', now(), now() + interval '1 hour', 'failed', now(),
           jsonb_strip_nulls(jsonb_build_object(
           'request', jsonb_build_object(
             'configRevision', 1,
             'protocol', 'openai',
             'endpoint', '/v1/responses',
             'compact', false),
           'routing', jsonb_build_object(
             'scope', 'all',
             'groupRefs', '{}'::text[],
             'groupNamesSnapshot', '[]'::jsonb)))
         )",
    )
    .execute(pool)
    .await
    .expect("seed request without optional facts");
}

async fn seed_request_at_27(pool: &sqlx::PgPool) {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, started_at, deadline_at, outcome, completed_at, request_observation_json
         ) values (
           'req_integrity', 'deleted_key', 'responses', 'http_sse', now(), now() + interval '1 hour', 'failed', now(),
           jsonb_strip_nulls(jsonb_build_object(
           'request', jsonb_build_object(
             'configRevision', 1,
             'protocol', 'openai',
             'endpoint', '/v1/responses',
             'compact', false),
           'routing', jsonb_build_object(
             'scope', 'all',
             'groupRefs', '{}'::text[],
             'groupNamesSnapshot', '[]'::jsonb)))
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
        "request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'scheduling', coalesce(request_observation_json #> '{scheduling}', '{}'::jsonb) || jsonb_build_object(
             'capacityUsedSlots', 1)))",
        "request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'scheduling', coalesce(request_observation_json #> '{scheduling}', '{}'::jsonb) || jsonb_build_object(
             'capacityTotalSlots', 2)))",
        "request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'transport', coalesce(request_observation_json #> '{transport}', '{}'::jsonb) || jsonb_build_object(
             'connection', coalesce(request_observation_json #> '{transport,connection}', '{}'::jsonb) || jsonb_build_object(
               'id', 'connection'))))",
        "request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'transport', coalesce(request_observation_json #> '{transport}', '{}'::jsonb) || jsonb_build_object(
             'connection', coalesce(request_observation_json #> '{transport,connection}', '{}'::jsonb) || jsonb_build_object(
               'id', 'connection',
               'exitReason', 'peer_closed',
               'ageMs', 10))))",
        "recovery_request_id = 'req_recovery', recovered_at = completed_at, recovery_attempt_count = 1",
        "recovery_request_id = 'req_recovery',
           recovered_at = completed_at,
           recovery_attempt_count = 1,
           request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'recovery', coalesce(request_observation_json #> '{recovery}', '{}'::jsonb) || jsonb_build_object(
             'retryDelayMs', 0)))",
        "outcome = 'running',
           completed_at = null,
           recovered_at = now(),
           recovery_request_id = 'req_recovery',
           recovery_attempt_count = 1,
           request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'recovery', coalesce(request_observation_json #> '{recovery}', '{}'::jsonb) || jsonb_build_object(
             'retryDelayMs', 0,
             'totalLatencyMs', 1)))",
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
        "update model_requests set cost_source = 'calculated',
           cost_amount = 1,
           cost_currency = 'USD',
           recovery_request_id = 'req_recovery',
           recovered_at = completed_at,
           recovery_attempt_count = 1,
           request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'scheduling', coalesce(request_observation_json #> '{scheduling}', '{}'::jsonb) || jsonb_build_object(
             'capacityUsedSlots', 1,
             'capacityTotalSlots', 2),
           'transport', coalesce(request_observation_json #> '{transport}', '{}'::jsonb) || jsonb_build_object(
             'connection', coalesce(request_observation_json #> '{transport,connection}', '{}'::jsonb) || jsonb_build_object(
               'id', 'connection',
               'exitReason', 'peer_closed',
               'ageMs', 10,
               'idleMs', 2)),
           'recovery', coalesce(request_observation_json #> '{recovery}', '{}'::jsonb) || jsonb_build_object(
             'retryDelayMs', 0,
             'totalLatencyMs', 1)))
         where id = 'req_integrity'",
    )
    .execute(&db.pool)
    .await
    .expect("complete observations satisfy presence and value constraints");
    db.close().await;
}

#[tokio::test]
async fn historical_references_are_validated_without_live_foreign_keys() {
    let Some(db) = TestDatabase::create("account_fact_refs").await else {
        return;
    };
    seed_request(&db.pool).await;
    let error = sqlx::query("update model_requests set provider_account_ref = ''")
        .execute(&db.pool)
        .await
        .expect_err("empty historical reference must fail");
    assert_check_rejected(&error);
    let error = sqlx::query("insert into ops_events (id, level, component, operation, provider_account_ref, failure_kind, message, created_at)
        values ('ops_empty_ref', 'error', 'probe', 'probe', '', 'timeout', 'test', now())")
        .execute(&db.pool).await.expect_err("empty event reference must fail");
    assert_check_rejected(&error);
    sqlx::query("update model_requests set provider_account_ref = 'already_deleted_account'")
        .execute(&db.pool)
        .await
        .expect("history does not require a live account");
    let references: String = sqlx::query_scalar(
        "select provider_account_ref from model_requests where id = 'req_integrity'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(references, "already_deleted_account");
    let live_columns: i64 = sqlx::query_scalar("select count(*) from information_schema.columns where table_schema = current_schema()
        and ((table_name = 'model_requests' and column_name in ('client_api_key_id','provider_account_id'))
          or (table_name = 'ops_events' and column_name = 'provider_account_id'))")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(live_columns, 0);
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

#[tokio::test]
async fn request_observation_migration_preserves_every_historical_field() {
    let Some(db) = TestDatabase::create_through("request_observation_upgrade", 24).await else {
        return;
    };
    seed_legacy_request(&db.pool).await;
    sqlx::raw_sql(
        "insert into model_requests
           select (jsonb_populate_record(null::model_requests,
             to_jsonb(mr) || jsonb_build_object('id', 'req_empty'))).*
           from model_requests mr;
         update model_requests set config_revision = 7,
           provider_account_name_snapshot = 'historical account',
           provider_account_email_snapshot = 'history@example.invalid',
           provider_account_authentication_kind_snapshot = 'oauth',
           routing_scope = 'groups', routing_group_refs = array['deleted_group'],
           routing_group_names_snapshot = '[\"历史组\"]',
           provider_error_code = 'rate_limit', error_message = 'safe error', retry_after_ms = 0,
           client_ip = '192.0.2.10/24', user_agent = 'fixture', reasoning_effort = 'high',
           reasoning_preset = 'balanced', subagent_kind = 'review', compact = true,
           transport_decision_wait_ms = 0, connect_ms = 2, headers_ms = 3,
           first_event_ms = 4, first_reasoning_ms = 5, first_text_ms = 6,
           first_token_ms = 5, latency_ms = 1000, provider_processing_ms = 7,
           admission_decision_ms = 0, account_selection_wait_ms = 8,
           capacity_used_slots = 0, capacity_total_slots = 20,
           http_version = 'HTTP/2', websocket_pool = 'reuse',
           upstream_connection_id = 'connection', upstream_connection_exit_reason = 'peer_closed',
           upstream_connection_age_ms = 10, upstream_connection_idle_ms = 0,
           upstream_response_model = 'response-model', continuation_requested = true,
           continuation_previous_response_id_hash = repeat('a', 64),
           continuation_unavailable_reason = 'missing_response',
           recovery_request_id = 'recovery', recovered_at = completed_at + interval '1 second',
           recovery_attempt_count = 1, recovery_retry_delay_ms = 0, recovery_total_latency_ms = 1000,
           input_tokens = 10, output_tokens = 20, total_tokens = 30,
           cost_source = 'provider_reported', cost_amount = 0.0123456789, cost_currency = 'USD',
           error_details = '{ \"opaque\": true }', provider_observation_json = '{\"vendor\":true}',
           diagnostic_trace_json = '{\"attempts\":[]}'
         where id = 'req_integrity'",
    ).execute(&db.pool).await.expect("populate all moved fields in the released schema");
    let before: Vec<serde_json::Value> =
        sqlx::query_scalar("select to_jsonb(mr) from model_requests mr order by id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    super::TEST_MIGRATOR.run_to(27, &db.pool).await.unwrap();
    let after: Vec<serde_json::Value> = sqlx::query_scalar(
        "select to_jsonb(mr) - 'request_observation_json' - 'upstream_response_ms' - 'upstream_api_overhead_ms' - 'upstream_engine_ms' - 'upstream_engine_iapi_ttft_ms' - 'upstream_engine_service_ttft_ms' - 'upstream_engine_iapi_tbt_ms' - 'upstream_engine_service_tbt_ms'
         from model_request_observations mr order by id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        after, before,
        "all 90 historical facts survive the migration"
    );
    let columns: i64 = sqlx::query_scalar(
        "select count(*) from information_schema.columns
         where table_schema = current_schema() and table_name = 'model_requests'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(columns, 49);
    let observation: serde_json::Value = sqlx::query_scalar(
        "select request_observation_json from model_requests where id = 'req_integrity'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        observation["timings"]["local"]["transportDecisionWaitMs"],
        0
    );
    assert_eq!(observation["scheduling"]["capacityUsedSlots"], 0);
    assert!(
        observation["timings"]["upstream"]
            .get("responseMs")
            .is_none()
    );
    db.close().await;
}

#[tokio::test]
async fn request_observation_reject_malformed_documents_and_keep_upstream_clock_independent() {
    use serde_json::json;

    let Some(db) = TestDatabase::create("request_observation_shape").await else {
        return;
    };
    seed_request(&db.pool).await;
    let original: serde_json::Value =
        sqlx::query_scalar("select request_observation_json from model_requests")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    for patch in [
        json!({"unknown": {}}),
        json!({"timings": []}),
        json!({"request": {}}),
        json!({"request": null}),
        json!({"routing": {"scope": "all", "groupRefs": [null], "groupNamesSnapshot": []}}),
        json!({"timings": {"local": {"latencyMs": -1}}}),
        json!({"timings": {"local": {"latencyMs": 1.0}}}),
        json!({"timings": {"local": {"latencyMs": "1"}}}),
        json!({"timings": {"local": {"latencyMs": u64::MAX}}}),
        json!({"timings": {"upstream": {"responseMs": 0}}}),
        json!({"timings": {"upstream": {"engineMs": -0.5}}}),
        json!({"timings": {"upstream": {"engineIapiTbtMs": "1.5"}}}),
        json!({"timings": {"upstream": {"engineServiceTbtMs": 1e99}}}),
        json!({"transport": {"websocketPool": "invalid"}}),
        json!({"continuation": {"unavailableReason": "invalid reason"}}),
        json!({"error": {"message": "x".repeat(1024 * 1024)}}),
    ] {
        let mut document = original.clone();
        document
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        sqlx::query("update model_requests set request_observation_json = $1")
            .bind(sqlx::types::Json(document))
            .execute(&db.pool)
            .await
            .expect_err("invalid JSON must not enter the query projection");
    }
    let mut invalid_ip = original.clone();
    invalid_ip["request"]["clientIp"] = json!("not an IP address");
    sqlx::query("update model_requests set request_observation_json = $1")
        .bind(sqlx::types::Json(invalid_ip))
        .execute(&db.pool)
        .await
        .expect_err("invalid IP");
    sqlx::query("update model_requests set request_observation_json = request_observation_json || $1::jsonb")
        .bind(sqlx::types::Json(json!({"timings": {
            "local": {"latencyMs": 1, "firstTokenMs": 0},
            "upstream": {"processingMs": 500, "responseMs": 1000, "engineIapiTbtMs": 2.450638, "apiOverheadMs": 0.0}
        }})))
        .execute(&db.pool)
        .await
        .expect("upstream has an independent clock");
    let timing: (i64, i64, i64, Option<i64>) = sqlx::query_as(
        "select first_token_ms, provider_processing_ms, upstream_response_ms, connect_ms
         from model_request_observations",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(timing, (0, 500, 1000, None));
    let metrics: (f64, f64, Option<f64>) = sqlx::query_as(
        "select upstream_engine_iapi_tbt_ms, upstream_api_overhead_ms, upstream_engine_ms from model_request_observations",
    ).fetch_one(&db.pool).await.unwrap();
    assert_eq!(metrics, (2.450638, 0.0, None));
    db.close().await;
}

#[tokio::test]
async fn observation_validation_upgrade_preserves_history_and_rejects_invalid_writes() {
    let Some(db) = TestDatabase::create_through("observation_validation_upgrade", 27).await else {
        return;
    };
    seed_request_at_27(&db.pool).await;
    let before: serde_json::Value =
        sqlx::query_scalar("select to_jsonb(mr) from model_requests mr")
            .fetch_one(&db.pool)
            .await
            .unwrap();

    super::TEST_MIGRATOR.run_to(28, &db.pool).await.unwrap();
    let after: serde_json::Value = sqlx::query_scalar("select to_jsonb(mr) from model_requests mr")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(after, before);

    for statement in [
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, started_at, deadline_at,
           outcome, completed_at, request_observation_json
         ) select 'invalid_insert', client_api_key_ref, operation, client_transport,
           started_at, deadline_at, outcome, completed_at,
           request_observation_json || '{\"unknown\":true}'::jsonb from model_requests",
        "update model_requests set request_observation_json =
           request_observation_json || '{\"unknown\":true}'::jsonb",
    ] {
        let error = sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&db.pool)
            .await
            .expect_err("invalid observations must still fail immediately");
        assert_check_rejected(&error);
        assert_eq!(
            error.as_database_error().unwrap().constraint(),
            Some("model_requests_observation_shape_ck")
        );
    }
    let rows: Vec<serde_json::Value> =
        sqlx::query_scalar("select to_jsonb(mr) from model_requests mr")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![before]);
    db.close().await;
}

#[tokio::test]
async fn unchanged_observations_do_not_repeat_full_document_validation() {
    let Some(db) = TestDatabase::create("observation_validation_cost").await else {
        return;
    };
    // 统计原校验器调用次数，避免性能回归断言依赖 CI 机器的运行速度
    sqlx::query("create sequence observation_validation_calls")
        .execute(&db.pool)
        .await
        .unwrap();
    let definition: String = sqlx::query_scalar(
        "select pg_get_functiondef('model_request_observation_valid(jsonb)'::regprocedure)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let instrumented = definition.replacen(
        "begin\n",
        "begin\n  perform nextval('observation_validation_calls');\n",
        1,
    );
    assert_ne!(instrumented, definition);
    sqlx::raw_sql(sqlx::AssertSqlSafe(instrumented))
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("create sequence observation_business_calls")
        .execute(&db.pool)
        .await
        .unwrap();
    let business_definition: String = sqlx::query_scalar(
        "select pg_get_functiondef('check_model_request_observation()'::regprocedure)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let business_instrumented = business_definition.replacen(
        "begin\n",
        "begin\n  perform nextval('observation_business_calls');\n",
        1,
    );
    assert_ne!(business_definition, business_instrumented);
    sqlx::raw_sql(sqlx::AssertSqlSafe(business_instrumented))
        .execute(&db.pool)
        .await
        .unwrap();
    seed_request(&db.pool).await;
    sqlx::query("update model_requests set recovery_lease_expires_at = recovery_lease_expires_at + interval '1 second'")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update model_requests set request_observation_json = request_observation_json")
        .execute(&db.pool)
        .await
        .unwrap();
    let calls: i64 = sqlx::query_scalar("select last_value from observation_validation_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(calls, 1, "only the initial insert needs full validation");
    let business_calls: i64 =
        sqlx::query_scalar("select last_value from observation_business_calls")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        business_calls, 1,
        "unrelated updates must not enter business JSON validation"
    );

    sqlx::query(
        "update model_requests set request_observation_json = jsonb_set(
           request_observation_json, '{request,userAgent}', '\"changed\"'::jsonb)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let calls: i64 = sqlx::query_scalar("select last_value from observation_validation_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        calls, 2,
        "changed observations must run the original validator"
    );
    db.close().await;
}

#[tokio::test]
async fn observation_validation_checks_the_final_row_after_before_triggers() {
    let Some(db) = TestDatabase::create("observation_validation_final_row").await else {
        return;
    };
    seed_request(&db.pool).await;
    sqlx::raw_sql(
        "create function change_observation_before_update() returns trigger language plpgsql as $$
         begin
           new.request_observation_json := new.request_observation_json || '{\"unknown\":true}'::jsonb;
           return new;
         end;
         $$;
         create trigger change_observation_before_update before update on model_requests
           for each row execute function change_observation_before_update();",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let error =
        sqlx::query("update model_requests set recovery_lease_expires_at = recovery_lease_expires_at + interval '1 second'")
            .execute(&db.pool)
            .await
            .expect_err("indirect JSON changes must not bypass validation");
    assert_check_rejected(&error);
    db.close().await;
}

#[tokio::test]
async fn conditional_business_validation_preserves_each_constraint_and_cross_column_updates() {
    let Some(db) = TestDatabase::create_through("observation_dependencies", 28).await else {
        return;
    };
    seed_request_at_27(&db.pool).await;
    let cases = [
        (
            "model_requests_upstream_duration_ck",
            "request_observation_json = request_observation_json || '{\"timings\":{\"upstream\":{\"responseMs\":0}}}'::jsonb",
        ),
        ("model_requests_revision_attempt_ck", "attempt_count = -1"),
        ("model_requests_status_ck", "client_status_code = 99"),
        (
            "model_requests_websocket_pool_ck",
            "request_observation_json = request_observation_json || '{\"transport\":{\"websocketPool\":\"invalid\"}}'::jsonb",
        ),
        (
            "model_requests_routing_group_names_ck",
            "request_observation_json = jsonb_set(request_observation_json, '{routing,scope}', '\"invalid\"')",
        ),
        (
            "model_requests_routing_group_names_ck",
            "request_observation_json = request_observation_json || '{\"routing\":{\"scope\":\"groups\",\"groupRefs\":[\"group\"],\"groupNamesSnapshot\":[]}}'::jsonb",
        ),
        (
            "model_requests_latency_ck",
            "request_observation_json = request_observation_json || '{\"timings\":{\"local\":{\"latencyMs\":1,\"firstTokenMs\":2}}}'::jsonb",
        ),
        (
            "model_requests_runtime_pressure_ck",
            "request_observation_json = request_observation_json || '{\"scheduling\":{\"capacityUsedSlots\":3,\"capacityTotalSlots\":2}}'::jsonb",
        ),
        (
            "model_requests_continuation_hashes_ck",
            "continuation_affinity_hash = 'invalid'",
        ),
        (
            "model_requests_continuation_reason_ck",
            "request_observation_json = request_observation_json || '{\"continuation\":{\"unavailableReason\":\"invalid reason\"}}'::jsonb",
        ),
        (
            "model_requests_upstream_connection_ck",
            "request_observation_json = request_observation_json || '{\"transport\":{\"connection\":{\"id\":\"connection\",\"exitReason\":\"closed\",\"ageMs\":1,\"idleMs\":2}}}'::jsonb",
        ),
        ("model_requests_recovery_ck", "recovery_attempt_count = -1"),
        (
            "model_requests_fact_completeness_ck",
            "cost_source = 'calculated', cost_amount = 1",
        ),
    ];
    for version in [28, 29] {
        super::TEST_MIGRATOR
            .run_to(version, &db.pool)
            .await
            .unwrap();
        for (constraint, assignment) in cases {
            let error = sqlx::query(sqlx::AssertSqlSafe(format!(
                "update model_requests set {assignment} where id = 'req_integrity'"
            )))
            .execute(&db.pool)
            .await
            .expect_err("invalid final row must fail");
            assert_check_rejected(&error);
            assert_eq!(
                error.as_database_error().unwrap().constraint(),
                Some(constraint),
                "migration {version}: {assignment}"
            );
        }
    }
    sqlx::query("update model_requests set continuation_requested = true, request_observation_json = request_observation_json || jsonb_build_object('continuation', jsonb_build_object('previousResponseIdHash', repeat('a',64)))")
        .execute(&db.pool).await.unwrap();
    let error = sqlx::query("update model_requests set continuation_requested = false")
        .execute(&db.pool)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("model_requests_continuation_hashes_ck")
    );
    db.close().await;
}

#[tokio::test]
async fn lease_migration_moves_running_expiry_and_preserves_terminal_expiry() {
    let Some(db) = TestDatabase::create_through("lease_upgrade", 30).await else {
        return;
    };
    seed_request_at_27(&db.pool).await;
    sqlx::query("insert into model_requests (id, client_api_key_ref, operation, client_transport, started_at, deadline_at, request_observation_json)
        select 'running_upgrade', client_api_key_ref, operation, client_transport, started_at, deadline_at, request_observation_json from model_requests where id = 'req_integrity'")
        .execute(&db.pool).await.unwrap();
    let expiry: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select deadline_at from model_requests where id = 'running_upgrade'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    super::TEST_MIGRATOR.run_to(31, &db.pool).await.unwrap();
    let running: (Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>) = sqlx::query_as("select request.recovery_lease_expires_at, lease.lease_expires_at from model_requests request join model_request_leases lease on lease.request_id = request.id where request.id = 'running_upgrade'")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(running, (None, expiry));
    let terminal: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "select recovery_lease_expires_at from model_requests where id = 'req_integrity'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(terminal, expiry);
    db.close().await;
}
