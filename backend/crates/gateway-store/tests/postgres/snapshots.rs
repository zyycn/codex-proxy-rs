use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::postgres::{
    DiagnosticDimension, ModelRequestAttemptStart, ModelRequestRepository, NewModelRequest,
    ObservabilityPageSize, ObservabilityRange, ObservabilityRepository, OpsErrorFilter,
    OpsErrorQuery, OpsEvent, OpsEventLevel, OpsEventRepository, PgExecutionStore,
    PgOpsEventRepository, ProviderAccountUsageQuery, UsageRecordFilter, UsageRecordQuery,
};
use sqlx::PgPool;

use super::{TestDatabase, observability_repository};

#[tokio::test]
async fn completed_usage_projections_should_accept_statusless_websocket_but_reject_statusless_http()
{
    let Some(database) = TestDatabase::create("snapshot_statusless_websocket_usage").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_statusless_transport",
        "Statusless transport",
        None,
        "oauth",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());

    let mut websocket_request = new_request("req_statusless_websocket", started_at);
    websocket_request.client_transport = "websocket".to_owned();
    store
        .insert_model_request_with_first_attempt(
            websocket_request,
            attempt("req_statusless_websocket", 1, "acct_statusless_transport"),
        )
        .await
        .expect("insert statusless WebSocket request");
    store
        .insert_model_request_with_first_attempt(
            new_request("req_statusless_http", started_at),
            attempt("req_statusless_http", 1, "acct_statusless_transport"),
        )
        .await
        .expect("insert statusless HTTP request");
    finalize_request_without_client_status(&database.pool, "req_statusless_websocket", started_at)
        .await;
    finalize_request_without_client_status(&database.pool, "req_statusless_http", started_at).await;

    let repository = observability_repository(&database.pool);
    let page = repository
        .list_usage_records(usage_query(started_at, UsageRecordFilter::default()))
        .await
        .expect("list statusless transport usage records");
    let request_ids = page
        .items
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        (page.total, request_ids),
        (1, vec!["req_statusless_websocket"])
    );

    let overview = repository
        .usage_summary(range_around(started_at), UsageRecordFilter::default())
        .await
        .expect("summarize statusless transport usage");
    assert_eq!(
        (
            overview.requests.request_count,
            overview.requests.success_count,
            overview.requests.total_tokens,
            overview.attempts.cost_coverage.provider_reported_count,
        ),
        (2, 2, 10, 1),
    );

    let account_usage = repository
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(
                range_around(started_at),
                vec!["acct_statusless_transport".to_owned()],
            )
            .expect("statusless account usage query")
            .with_hourly_request_buckets()
            .expect("statusless account request timeline"),
        )
        .await
        .expect("load statusless account usage");
    assert_eq!(
        (
            account_usage[0].request_count,
            account_usage[0].total_tokens,
            account_usage[0].models[0].request_count,
            account_usage[0]
                .request_buckets
                .iter()
                .map(|bucket| bucket.request_count)
                .sum::<u64>(),
            account_usage[0].costs[0].amount.as_str(),
        ),
        (1, Some(10), 1, 1, "1.25"),
    );

    let diagnostics = repository
        .usage_diagnostics(
            range_around(started_at),
            UsageRecordFilter::default(),
            DiagnosticDimension::Account,
        )
        .await
        .expect("load statusless usage diagnostics");
    assert_eq!(
        (
            diagnostics[0].request_count,
            diagnostics[0].success_count,
            diagnostics[0].total_tokens,
            diagnostics[0].average_latency_ms,
            diagnostics[0].cost_coverage.provider_reported_count,
            diagnostics[0].costs[0].amount.as_str(),
        ),
        (2, 2, 10, Some(500), 1, "1.25"),
    );

    database.close().await;
}

#[tokio::test]
async fn request_snapshots_should_survive_account_deletion() {
    let Some(database) = TestDatabase::create("snapshot_request_delete").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snap_a",
        "Snapshot Alpha",
        Some("alpha@example.invalid"),
        "oauth",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_a", started_at),
            attempt("req_snap_a", 1, "acct_snap_a"),
        )
        .await
        .expect("insert request with first attempt");
    finalize_request(&database.pool, "req_snap_a", started_at).await;

    sqlx::query("delete from provider_accounts where id = 'acct_snap_a'")
        .execute(&database.pool)
        .await
        .expect("delete provider account");

    let repository = observability_repository(&database.pool);
    let page = repository
        .list_usage_records(usage_query(started_at, UsageRecordFilter::default()))
        .await
        .expect("list usage records after deletion");
    let record = page
        .items
        .iter()
        .find(|record| record.id == "req_snap_a")
        .expect("request snapshot record");
    assert_eq!(
        (
            record.provider_account_ref.as_deref(),
            record.provider_account_name.as_deref(),
            record.provider_account_email.as_deref(),
            record.provider_account_authentication_kind.as_deref(),
        ),
        (
            Some("acct_snap_a"),
            Some("Snapshot Alpha"),
            Some("alpha@example.invalid"),
            Some("oauth"),
        )
    );

    let detail = repository
        .usage_record_detail("req_snap_a")
        .await
        .expect("usage record detail after deletion");
    assert_eq!(
        (
            detail.request.provider_account_name.as_deref(),
            detail.request.provider_account_email.as_deref(),
            detail
                .request
                .provider_account_authentication_kind
                .as_deref(),
        ),
        (
            Some("Snapshot Alpha"),
            Some("alpha@example.invalid"),
            Some("oauth"),
        )
    );

    database.close().await;
}

#[tokio::test]
async fn attempts_should_keep_their_own_account_snapshots() {
    let Some(database) = TestDatabase::create("snapshot_attempt").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snap_a",
        "Snapshot Alpha",
        Some("alpha@example.invalid"),
        "oauth",
        started_at,
    )
    .await;
    seed_account(
        &database.pool,
        "acct_snap_b",
        "Snapshot Beta",
        Some("beta@example.invalid"),
        "api_key",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_ab", started_at),
            attempt("req_snap_ab", 1, "acct_snap_a"),
        )
        .await
        .expect("insert first attempt");
    PgOpsEventRepository::new(database.pool.clone())
        .append_ops_event(OpsEvent {
            id: "ops_snap_a".to_owned(),
            model_request_id: Some("req_snap_ab".to_owned()),
            attempt_index: Some(1),
            level: OpsEventLevel::Warning,
            component: "routing".to_owned(),
            operation: "fallback".to_owned(),
            provider_kind: Some("openai".to_owned()),
            provider_account_id: Some("acct_snap_a".to_owned()),
            provider_account_ref: Some("acct_snap_a".to_owned()),
            upstream_model_id: Some("upstream-a".to_owned()),
            failure_kind: "rate_limited".to_owned(),
            status_code: Some(429),
            provider_error_code: Some("rate_limit".to_owned()),
            retry_after_ms: Some(1_000),
            upstream_request_id: None,
            latency_ms: Some(120),
            message: "first account was limited".to_owned(),
            occurrence_count: 1,
            created_at: started_at + chrono::Duration::seconds(1),
        })
        .await
        .expect("append intermediate failure");
    store
        .begin_model_request_attempt(attempt("req_snap_ab", 2, "acct_snap_b"))
        .await
        .expect("begin second attempt");
    finalize_request(&database.pool, "req_snap_ab", started_at).await;

    sqlx::query("delete from provider_accounts where id in ('acct_snap_a', 'acct_snap_b')")
        .execute(&database.pool)
        .await
        .expect("delete provider accounts");

    let repository = observability_repository(&database.pool);
    let detail = repository
        .usage_record_detail("req_snap_ab")
        .await
        .expect("usage detail with mixed attempts");
    let intermediate = detail
        .attempts
        .iter()
        .find(|attempt| attempt.source == "ops_event")
        .expect("intermediate attempt");
    assert_eq!(
        (
            intermediate.provider_account_name.as_deref(),
            intermediate.provider_account_email.as_deref(),
            intermediate.provider_account_authentication_kind.as_deref(),
        ),
        (
            Some("Snapshot Alpha"),
            Some("alpha@example.invalid"),
            Some("oauth"),
        )
    );
    let final_attempt = detail
        .attempts
        .iter()
        .find(|attempt| attempt.source == "model_request")
        .expect("final attempt");
    assert_eq!(
        (
            final_attempt.provider_account_name.as_deref(),
            final_attempt.provider_account_email.as_deref(),
            final_attempt
                .provider_account_authentication_kind
                .as_deref(),
        ),
        (
            Some("Snapshot Beta"),
            Some("beta@example.invalid"),
            Some("api_key"),
        )
    );
    assert_eq!(
        detail.request.provider_account_email.as_deref(),
        Some("beta@example.invalid")
    );

    database.close().await;
}

#[tokio::test]
async fn ops_errors_should_keep_request_and_event_snapshots_after_account_deletion() {
    let Some(database) = TestDatabase::create("snapshot_ops_errors").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snap_a",
        "Snapshot Alpha",
        Some("alpha@example.invalid"),
        "oauth",
        started_at,
    )
    .await;
    seed_account(
        &database.pool,
        "acct_snap_c",
        "Snapshot Charlie",
        Some("charlie@example.invalid"),
        "api_key",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_error", started_at),
            attempt("req_snap_error", 1, "acct_snap_a"),
        )
        .await
        .expect("insert failed request");
    sqlx::query(
        "update model_requests
         set outcome = 'failed', error_kind = 'upstream_error',
             error_message = 'snapshot failure', client_status_code = 502,
             upstream_status_code = 502, completed_at = $1
         where id = 'req_snap_error'",
    )
    .bind(started_at + chrono::Duration::seconds(2))
    .execute(&database.pool)
    .await
    .expect("mark request failed");
    PgOpsEventRepository::new(database.pool.clone())
        .append_ops_event(OpsEvent {
            id: "ops_snap_probe".to_owned(),
            model_request_id: None,
            attempt_index: None,
            level: OpsEventLevel::Warning,
            component: "account_probe".to_owned(),
            operation: "connection_test".to_owned(),
            provider_kind: Some("xai".to_owned()),
            provider_account_id: Some("acct_snap_c".to_owned()),
            provider_account_ref: Some("acct_snap_c".to_owned()),
            upstream_model_id: Some("grok-test".to_owned()),
            failure_kind: "auth_failed".to_owned(),
            status_code: Some(401),
            provider_error_code: Some("invalid_api_key".to_owned()),
            retry_after_ms: None,
            upstream_request_id: None,
            latency_ms: Some(80),
            message: "probe failed".to_owned(),
            occurrence_count: 1,
            created_at: started_at + chrono::Duration::seconds(3),
        })
        .await
        .expect("append probe failure");

    sqlx::query("delete from provider_accounts where id in ('acct_snap_a', 'acct_snap_c')")
        .execute(&database.pool)
        .await
        .expect("delete provider accounts");

    let repository = observability_repository(&database.pool);
    let errors = repository
        .list_ops_errors(OpsErrorQuery {
            range: range_around(started_at),
            filter: OpsErrorFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("list ops errors after deletion");
    let request_error = errors
        .items
        .iter()
        .find(|error| error.source == "model_request")
        .expect("request error");
    assert_eq!(
        (
            request_error.provider_account_name.as_deref(),
            request_error.provider_account_email.as_deref(),
            request_error
                .provider_account_authentication_kind
                .as_deref(),
        ),
        (
            Some("Snapshot Alpha"),
            Some("alpha@example.invalid"),
            Some("oauth"),
        )
    );
    assert_eq!(request_error.endpoint.as_deref(), Some("/v1/responses"));
    let probe_error = errors
        .items
        .iter()
        .find(|error| error.source == "ops_event")
        .expect("probe error");
    assert_eq!(
        (
            probe_error.provider_account_name.as_deref(),
            probe_error.provider_account_email.as_deref(),
            probe_error.provider_account_authentication_kind.as_deref(),
        ),
        (
            Some("Snapshot Charlie"),
            Some("charlie@example.invalid"),
            Some("api_key"),
        )
    );
    assert_eq!(probe_error.endpoint, None);

    database.close().await;
}

#[tokio::test]
async fn diagnostics_should_group_same_email_accounts_by_stable_ref() {
    let Some(database) = TestDatabase::create("snapshot_diagnostics_same_email").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snap_a",
        "Alpha",
        Some("same@example.invalid"),
        "oauth",
        started_at,
    )
    .await;
    seed_account(
        &database.pool,
        "acct_snap_b",
        "Beta",
        Some("same@example.invalid"),
        "api_key",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_a", started_at),
            attempt("req_snap_a", 1, "acct_snap_a"),
        )
        .await
        .expect("insert account a request");
    finalize_request(&database.pool, "req_snap_a", started_at).await;
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_b", started_at + chrono::Duration::seconds(1)),
            attempt("req_snap_b", 1, "acct_snap_b"),
        )
        .await
        .expect("insert account b request");
    finalize_request(&database.pool, "req_snap_b", started_at).await;

    sqlx::query("delete from provider_accounts where id in ('acct_snap_a', 'acct_snap_b')")
        .execute(&database.pool)
        .await
        .expect("delete provider accounts");

    let repository = observability_repository(&database.pool);
    let diagnostics = repository
        .usage_diagnostics(
            range_around(started_at),
            UsageRecordFilter::default(),
            DiagnosticDimension::Account,
        )
        .await
        .expect("account diagnostics");
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(
        diagnostics
            .iter()
            .map(|item| item.key.as_str())
            .collect::<Vec<_>>(),
        vec!["acct_snap_a", "acct_snap_b"]
    );
    assert!(
        diagnostics
            .iter()
            .all(|item| item.name == "same@example.invalid")
    );
    assert!(diagnostics.iter().all(|item| item.request_count == 1));

    database.close().await;
}

#[tokio::test]
async fn diagnostics_should_fallback_to_name_then_ref_for_missing_snapshots() {
    let Some(database) = TestDatabase::create("snapshot_diagnostics_fallback").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snap_c",
        "Snapshot Charlie",
        None,
        "api_key",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_c", started_at),
            attempt("req_snap_c", 1, "acct_snap_c"),
        )
        .await
        .expect("insert account c request");
    finalize_request(&database.pool, "req_snap_c", started_at).await;
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_legacy", started_at + chrono::Duration::seconds(1)),
            ModelRequestAttemptStart {
                account_selection_wait_ms: None,
                capacity_used_slots: None,
                capacity_total_slots: None,
                model_request_id: "req_snap_legacy".to_owned(),
                attempt_count: 1,
                provider_kind: "openai".to_owned(),
                provider_account_id: None,
                provider_account_ref: Some("acct_legacy_deleted".to_owned()),
                upstream_model_id: Some("legacy-model".to_owned()),
                upstream_transport: "http_sse".to_owned(),
                http_version: None,
            },
        )
        .await
        .expect("insert legacy request without live account");
    finalize_request(&database.pool, "req_snap_legacy", started_at).await;

    let repository = observability_repository(&database.pool);
    let diagnostics = repository
        .usage_diagnostics(
            range_around(started_at),
            UsageRecordFilter::default(),
            DiagnosticDimension::Account,
        )
        .await
        .expect("account diagnostics");
    let by_key = diagnostics
        .iter()
        .map(|item| (item.key.as_str(), item.name.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(by_key.get("acct_snap_c"), Some(&"Snapshot Charlie"));
    assert_eq!(
        by_key.get("acct_legacy_deleted"),
        Some(&"acct_legacy_deleted")
    );

    database.close().await;
}

#[tokio::test]
async fn diagnostics_should_prefer_the_latest_non_null_email_snapshot() {
    let Some(database) = TestDatabase::create("snapshot_diagnostics_latest_email").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snapshot_history",
        "Older Name",
        Some("history@example.invalid"),
        "oauth",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snapshot_history_old", started_at),
            attempt("req_snapshot_history_old", 1, "acct_snapshot_history"),
        )
        .await
        .expect("insert older account snapshot");
    finalize_request(&database.pool, "req_snapshot_history_old", started_at).await;
    sqlx::query(
        "update provider_accounts
         set name = 'Newer Name', email = null, updated_at = now()
         where id = 'acct_snapshot_history'",
    )
    .execute(&database.pool)
    .await
    .expect("update account snapshot source");
    store
        .insert_model_request_with_first_attempt(
            new_request(
                "req_snapshot_history_new",
                started_at + chrono::Duration::seconds(1),
            ),
            attempt("req_snapshot_history_new", 1, "acct_snapshot_history"),
        )
        .await
        .expect("insert newer account snapshot");
    finalize_request(&database.pool, "req_snapshot_history_new", started_at).await;

    let diagnostics = observability_repository(&database.pool)
        .usage_diagnostics(
            range_around(started_at),
            UsageRecordFilter::default(),
            DiagnosticDimension::Account,
        )
        .await
        .expect("account diagnostics");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].key, "acct_snapshot_history");
    assert_eq!(diagnostics[0].name, "history@example.invalid");
    assert_eq!(diagnostics[0].request_count, 2);
    database.close().await;
}

#[tokio::test]
async fn failure_diagnostics_should_only_include_errored_requests() {
    let Some(database) = TestDatabase::create("snapshot_failure_dimension").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snap_a",
        "Snapshot Alpha",
        Some("alpha@example.invalid"),
        "oauth",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_ok", started_at),
            attempt("req_snap_ok", 1, "acct_snap_a"),
        )
        .await
        .expect("insert healthy request");
    finalize_request(&database.pool, "req_snap_ok", started_at).await;
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_err", started_at + chrono::Duration::seconds(1)),
            attempt("req_snap_err", 1, "acct_snap_a"),
        )
        .await
        .expect("insert errored request");
    sqlx::query(
        "update model_requests
         set outcome = 'failed', error_kind = 'rate_limited',
             error_message = 'upstream limited', client_status_code = 429,
             upstream_status_code = 429, completed_at = $2
         where id = $1",
    )
    .bind("req_snap_err")
    .bind(started_at + chrono::Duration::seconds(6))
    .execute(&database.pool)
    .await
    .expect("mark request failed");

    let repository = observability_repository(&database.pool);
    let diagnostics = repository
        .usage_diagnostics(
            range_around(started_at),
            UsageRecordFilter::default(),
            DiagnosticDimension::Failure,
        )
        .await
        .expect("failure diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].key, "rate_limited");
    assert_eq!(diagnostics[0].request_count, 1);
    assert_eq!(diagnostics[0].failure_count, 1);
    assert_eq!(diagnostics[0].success_count, 0);

    database.close().await;
}

#[tokio::test]
async fn api_key_diagnostics_should_display_key_name_and_fallback_to_ref() {
    let Some(database) = TestDatabase::create("snapshot_api_key_dimension").await else {
        return;
    };
    let started_at = Utc::now();
    seed_api_key(&database.pool, "key_diag", "My Key", started_at).await;
    let store = PgExecutionStore::new(database.pool.clone());
    for id in ["req_key_a", "req_key_b"] {
        let mut request = new_request(id, started_at);
        request.client_api_key_id = Some("key_diag".to_owned());
        request.client_api_key_ref = "key_diag".to_owned();
        let attempt = ModelRequestAttemptStart {
            account_selection_wait_ms: None,
            capacity_used_slots: None,
            capacity_total_slots: None,
            model_request_id: id.to_owned(),
            attempt_count: 1,
            provider_kind: "openai".to_owned(),
            provider_account_id: None,
            provider_account_ref: Some("acct_key".to_owned()),
            upstream_model_id: Some("upstream-model".to_owned()),
            upstream_transport: "http_sse".to_owned(),
            http_version: None,
        };
        store
            .insert_model_request_with_first_attempt(request, attempt)
            .await
            .expect("insert api key request");
        finalize_request(&database.pool, id, started_at).await;
    }
    sqlx::query("update model_requests set latency_ms = 1000 where id = 'req_key_a'")
        .execute(&database.pool)
        .await
        .expect("set request a latency");
    sqlx::query("update model_requests set latency_ms = 2000 where id = 'req_key_b'")
        .execute(&database.pool)
        .await
        .expect("set request b latency");

    let repository = observability_repository(&database.pool);
    let diagnostics = repository
        .usage_diagnostics(
            range_around(started_at),
            UsageRecordFilter::default(),
            DiagnosticDimension::ApiKey,
        )
        .await
        .expect("api key diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].key, "key_diag");
    assert_eq!(diagnostics[0].name, "My Key");
    assert_eq!(diagnostics[0].request_count, 2);
    assert_eq!(diagnostics[0].latency_p95_ms, Some(1950));

    sqlx::query("delete from client_api_keys where id = 'key_diag'")
        .execute(&database.pool)
        .await
        .expect("delete api key");
    let diagnostics = repository
        .usage_diagnostics(
            range_around(started_at),
            UsageRecordFilter::default(),
            DiagnosticDimension::ApiKey,
        )
        .await
        .expect("api key diagnostics after deletion");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].key, "key_diag");
    assert_eq!(diagnostics[0].name, "key_diag");

    database.close().await;
}

#[tokio::test]
async fn renaming_account_should_not_rewrite_historical_snapshots() {
    let Some(database) = TestDatabase::create("snapshot_rename").await else {
        return;
    };
    let started_at = Utc::now();
    seed_account(
        &database.pool,
        "acct_snap_a",
        "Original Name",
        Some("original@example.invalid"),
        "oauth",
        started_at,
    )
    .await;
    let store = PgExecutionStore::new(database.pool.clone());
    store
        .insert_model_request_with_first_attempt(
            new_request("req_snap_a", started_at),
            attempt("req_snap_a", 1, "acct_snap_a"),
        )
        .await
        .expect("insert request with original snapshot");
    finalize_request(&database.pool, "req_snap_a", started_at).await;
    sqlx::query(
        "update provider_accounts
         set name = 'Renamed', email = 'renamed@example.invalid'
         where id = 'acct_snap_a'",
    )
    .execute(&database.pool)
    .await
    .expect("rename provider account");

    let repository = observability_repository(&database.pool);
    let page = repository
        .list_usage_records(usage_query(started_at, UsageRecordFilter::default()))
        .await
        .expect("list usage records after rename");
    let record = page
        .items
        .iter()
        .find(|record| record.id == "req_snap_a")
        .expect("historical request");
    assert_eq!(
        (
            record.provider_account_name.as_deref(),
            record.provider_account_email.as_deref(),
        ),
        (Some("Original Name"), Some("original@example.invalid"))
    );

    database.close().await;
}

fn new_request(id: &str, started_at: DateTime<Utc>) -> NewModelRequest {
    NewModelRequest {
        admission_decision_ms: None,
        id: id.to_owned(),
        client_api_key_id: None,
        client_api_key_ref: "key_snapshot".to_owned(),
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
        deadline_at: started_at + chrono::Duration::seconds(30),
    }
}

fn attempt(id: &str, count: u32, account_id: &str) -> ModelRequestAttemptStart {
    ModelRequestAttemptStart {
        account_selection_wait_ms: None,
        capacity_used_slots: None,
        capacity_total_slots: None,
        model_request_id: id.to_owned(),
        attempt_count: count,
        provider_kind: "openai".to_owned(),
        provider_account_id: Some(account_id.to_owned()),
        provider_account_ref: Some(account_id.to_owned()),
        upstream_model_id: Some("upstream-model".to_owned()),
        upstream_transport: "http_sse".to_owned(),
        http_version: None,
    }
}

fn usage_query(started_at: DateTime<Utc>, filter: UsageRecordFilter) -> UsageRecordQuery {
    UsageRecordQuery {
        range: range_around(started_at),
        filter,
        current_page: 1,
        page_size: ObservabilityPageSize::new(10).expect("page size"),
    }
}

fn range_around(started_at: DateTime<Utc>) -> ObservabilityRange {
    ObservabilityRange::new(
        started_at - TimeDelta::hours(1),
        started_at + TimeDelta::hours(1),
    )
    .expect("observability range")
}

async fn seed_account(
    pool: &PgPool,
    id: &str,
    name: &str,
    email: Option<&str>,
    authentication_kind: &str,
    now: DateTime<Utc>,
) {
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind,
           provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, enabled, credential_state,
           credential_observed_at, created_at, updated_at
         ) values (
           $1, 'openai', $2, $3, 'user-' || $1, null, null, $4,
           '{}'::jsonb, 1, false, $5 + interval '1 day', true, 'ready',
           $5, $5, $5
         )",
    )
    .bind(id)
    .bind(name)
    .bind(email)
    .bind(authentication_kind)
    .bind(now)
    .execute(pool)
    .await
    .expect("seed provider account");
}

async fn finalize_request(pool: &PgPool, id: &str, started_at: DateTime<Utc>) {
    sqlx::query(
        "update model_requests
         set outcome = 'succeeded', client_status_code = 200,
             upstream_status_code = 200, downstream_committed_at = $2, completed_at = $2
         where id = $1",
    )
    .bind(id)
    .bind(started_at + chrono::Duration::seconds(5))
    .execute(pool)
    .await
    .expect("finalize request");
}

async fn finalize_request_without_client_status(
    pool: &PgPool,
    id: &str,
    started_at: DateTime<Utc>,
) {
    sqlx::query(
        "update model_requests
         set outcome = 'succeeded', upstream_send_state = 'sent',
             client_status_code = null, upstream_status_code = 200,
             input_tokens = 7, output_tokens = 3, total_tokens = 10,
             first_token_ms = 100, latency_ms = 500,
             cost_source = 'provider_reported', cost_amount = 1.25,
             cost_currency = 'USD',
             downstream_committed_at = $2, completed_at = $2
         where id = $1",
    )
    .bind(id)
    .bind(started_at + chrono::Duration::seconds(5))
    .execute(pool)
    .await
    .expect("finalize request without client status");
}

async fn seed_api_key(pool: &PgPool, id: &str, name: &str, now: DateTime<Utc>) {
    sqlx::query(
        "insert into client_api_keys (id, name, key, enabled, created_at, updated_at)
         values ($1, $2, 'sk_' || $3, true, $4, $4)",
    )
    .bind(id)
    .bind(name)
    .bind("A".repeat(43))
    .bind(now)
    .execute(pool)
    .await
    .expect("seed api key");
}
