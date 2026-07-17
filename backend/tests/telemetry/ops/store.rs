use chrono::{Duration, Utc};
use codex_proxy_rs::telemetry::{
    ops::store::PgOpsErrorLogStore,
    ops::types::{OpsErrorFilter, OpsErrorLog},
};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn ops_store_filters_diagnostic_dimensions() {
    let (pool, _guard) = init_test_db("ops-error-filter").await;
    let store = PgOpsErrorLogStore::new(pool);
    let mut error = OpsErrorLog::new("upstream", "rate limited");
    error.id = "ops_error_matching".to_string();
    error.request_id = Some("req_error".to_string());
    error.client_api_key_id = Some("key_1".to_string());
    error.failure_class = Some("rate_limited".to_string());
    error.upstream_status_code = Some(429);
    error.transport = Some("http_sse".to_string());
    store.append(&error).await.unwrap();

    let page = store
        .list_page(
            OpsErrorFilter {
                client_api_key_id: Some("key_1".to_string()),
                failure_class: Some("rate_limited".to_string()),
                upstream_status_code: Some(429),
                search: Some("req_error".to_string()),
                ..OpsErrorFilter::default()
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, "ops_error_matching");
}

#[tokio::test]
async fn ops_store_should_apply_every_query_filter_dimension() {
    let (pool, _guard) = init_test_db("ops-error-filter-matrix").await;
    let store = PgOpsErrorLogStore::new(pool);
    let now = Utc::now();
    let mut matching = OpsErrorLog::new("response", "needle failure");
    matching.id = "ops_matrix_matching".to_string();
    matching.client_api_key_id = Some("key_matching".to_string());
    matching.provider = Some("provider_matching".to_string());
    matching.request_id = Some("request_matching".to_string());
    matching.account_id = Some("account_matching".to_string());
    matching.route = Some("/v1/matching".to_string());
    matching.model = Some("model_matching".to_string());
    matching.status_code = Some(503);
    matching.client_status_code = Some(502);
    matching.upstream_status_code = Some(503);
    matching.transport = Some("websocket".to_string());
    matching.attempt_index = Some(3);
    matching.failure_class = Some("matching_failure".to_string());
    matching.response_id = Some("response_matching".to_string());
    matching.upstream_request_id = Some("upstream_matching".to_string());
    matching.created_at = now;
    store.append(&matching).await.unwrap();

    let mut other = OpsErrorLog::new("other", "other failure");
    other.id = "ops_matrix_other".to_string();
    other.client_api_key_id = Some("key_other".to_string());
    other.provider = Some("provider_other".to_string());
    other.request_id = Some("request_other".to_string());
    other.account_id = Some("account_other".to_string());
    other.route = Some("/v1/other".to_string());
    other.model = Some("model_other".to_string());
    other.status_code = Some(500);
    other.client_status_code = Some(500);
    other.upstream_status_code = Some(500);
    other.transport = Some("http_sse".to_string());
    other.attempt_index = Some(4);
    other.failure_class = Some("other_failure".to_string());
    other.response_id = Some("response_other".to_string());
    other.upstream_request_id = Some("upstream_other".to_string());
    other.created_at = now - Duration::hours(2);
    store.append(&other).await.unwrap();

    let filters = vec![
        (
            "kind",
            OpsErrorFilter {
                kind: Some(matching.kind.clone()),
                ..Default::default()
            },
        ),
        (
            "client key",
            OpsErrorFilter {
                client_api_key_id: matching.client_api_key_id.clone(),
                ..Default::default()
            },
        ),
        (
            "provider",
            OpsErrorFilter {
                provider: matching.provider.clone(),
                ..Default::default()
            },
        ),
        (
            "request",
            OpsErrorFilter {
                request_id: matching.request_id.clone(),
                ..Default::default()
            },
        ),
        (
            "account",
            OpsErrorFilter {
                account_id: matching.account_id.clone(),
                ..Default::default()
            },
        ),
        (
            "route",
            OpsErrorFilter {
                route: matching.route.clone(),
                ..Default::default()
            },
        ),
        (
            "model",
            OpsErrorFilter {
                model: matching.model.clone(),
                ..Default::default()
            },
        ),
        (
            "status",
            OpsErrorFilter {
                status_code: matching.status_code,
                ..Default::default()
            },
        ),
        (
            "client status",
            OpsErrorFilter {
                client_status_code: matching.client_status_code,
                ..Default::default()
            },
        ),
        (
            "upstream status",
            OpsErrorFilter {
                upstream_status_code: matching.upstream_status_code,
                ..Default::default()
            },
        ),
        (
            "transport",
            OpsErrorFilter {
                transport: matching.transport.clone(),
                ..Default::default()
            },
        ),
        (
            "attempt",
            OpsErrorFilter {
                attempt_index: matching.attempt_index,
                ..Default::default()
            },
        ),
        (
            "failure",
            OpsErrorFilter {
                failure_class: matching.failure_class.clone(),
                ..Default::default()
            },
        ),
        (
            "response",
            OpsErrorFilter {
                response_id: matching.response_id.clone(),
                ..Default::default()
            },
        ),
        (
            "upstream request",
            OpsErrorFilter {
                upstream_request_id: matching.upstream_request_id.clone(),
                ..Default::default()
            },
        ),
        (
            "message search",
            OpsErrorFilter {
                search: Some("needle failure".to_string()),
                ..Default::default()
            },
        ),
        (
            "request search",
            OpsErrorFilter {
                search: matching.request_id.clone(),
                ..Default::default()
            },
        ),
        (
            "response search",
            OpsErrorFilter {
                search: matching.response_id.clone(),
                ..Default::default()
            },
        ),
        (
            "upstream search",
            OpsErrorFilter {
                search: matching.upstream_request_id.clone(),
                ..Default::default()
            },
        ),
        (
            "time window",
            OpsErrorFilter {
                start_time: Some(now - Duration::minutes(1)),
                end_time: Some(now + Duration::minutes(1)),
                ..Default::default()
            },
        ),
    ];
    for (dimension, filter) in filters {
        let page = store.list_page(filter, 1, 20).await.unwrap();
        assert_eq!(page.total, 1, "{dimension}");
        assert_eq!(page.items[0].id, matching.id, "{dimension}");
    }
}

#[tokio::test]
async fn ops_error_and_bucket_are_one_transaction() {
    let (pool, _guard) = init_test_db("ops-error-transaction").await;
    let store = PgOpsErrorLogStore::new(pool.clone());
    sqlx::raw_sql(
        "create function reject_error_bucket() returns trigger language plpgsql as $$
         begin raise exception 'bucket rejected'; end $$;
         create trigger reject_error_bucket before insert or update on request_time_buckets
         for each row execute function reject_error_bucket();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut error = OpsErrorLog::new("upstream", "must rollback");
    error.id = "ops_error_rolled_back".to_string();
    assert!(store.append(&error).await.is_err());
    let count: i64 = sqlx::query_scalar("select count(*) from ops_error_logs where id = $1")
        .bind(&error.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn error_bucket_service_tier_should_match_rebuild_semantics() {
    let (pool, _guard) = init_test_db("ops-error-tier-rebuild").await;
    sqlx::query(
        "insert into runtime_settings (
           id, refresh_margin_seconds, refresh_concurrency,
           max_concurrent_per_account, request_interval_ms, rotation_strategy, updated_at
         ) values (1, 3600, 2, 1, 0, 'smart', now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    let store = PgOpsErrorLogStore::new(pool.clone());
    let mut error = OpsErrorLog::new("upstream", "tier must not drift");
    error.id = "ops_error_tier".to_string();
    error.provider = Some("openai".to_string());
    error.account_id = Some("acct_tier".to_string());
    error.model = Some("gpt-5".to_string());
    error.service_tier = Some("priority".to_string());
    store.append(&error).await.unwrap();

    let live_tier: String =
        sqlx::query_scalar("select service_tier from request_time_buckets where error_count = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(live_tier, "__unknown__");

    codex_proxy_rs::telemetry::rebuild::rebuild_buckets(&pool)
        .await
        .unwrap();
    let rebuilt_tier: String =
        sqlx::query_scalar("select service_tier from request_time_buckets where error_count = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rebuilt_tier, live_tier);
}
