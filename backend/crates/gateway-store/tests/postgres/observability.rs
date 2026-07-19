use chrono::{TimeDelta, Utc};
use gateway_store::postgres::{
    DiagnosticDimension, ObservabilityPageNumber, ObservabilityPageSize, ObservabilityRange,
    ObservabilityRepository, OpsErrorFilter, OpsErrorQuery, PgObservabilityRepository,
    ProviderAccountUsageQuery, UsageRecordFilter, UsageRecordQuery,
};
use sqlx::PgPool;

use super::TestDatabase;

#[test]
fn observability_range_rejects_empty_window() {
    let now = Utc::now();
    assert!(ObservabilityRange::new(now, now).is_err());
}

#[test]
fn observability_range_accepts_full_configured_retention_window() {
    let now = Utc::now();
    let range = ObservabilityRange::new(now - TimeDelta::days(730), now)
        .expect("store range must not truncate configured retention");

    assert_eq!(range.start, now - TimeDelta::days(730));
    assert_eq!(range.end, now);
}

#[test]
fn postgres_observability_adapter_implements_query_port() {
    fn assert_port<T: ObservabilityRepository>() {}
    assert_port::<PgObservabilityRepository>();
}

#[tokio::test]
async fn observability_queries_preserve_request_account_cost_and_diagnostic_facts() {
    let Some(database) = TestDatabase::create("observability").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = PgObservabilityRepository::new(database.pool.clone());

    let dashboard = repository
        .dashboard_summary(range)
        .await
        .expect("dashboard summary");
    assert_eq!(dashboard.requests.request_count, 2);
    assert_eq!(dashboard.requests.cache_eligible_request_count, 2);
    assert_eq!(dashboard.requests.cache_hit_request_count, 1);
    assert_eq!(dashboard.requests.cache_hit_request_rate(), Some(0.5));
    assert_eq!(
        dashboard
            .requests
            .latency_percentiles
            .p50_ms
            .expect("latency p50")
            .as_f64(),
        800.0
    );
    assert_eq!(
        dashboard
            .requests
            .latency_percentiles
            .p95_ms
            .expect("latency p95")
            .as_f64(),
        890.0
    );
    assert_eq!(
        dashboard
            .requests
            .latency_percentiles
            .p99_ms
            .expect("latency p99")
            .as_f64(),
        898.0
    );
    assert_eq!(
        dashboard
            .requests
            .first_token_latency_percentiles
            .p50_ms
            .expect("first token p50")
            .as_f64(),
        120.0
    );
    assert_eq!(dashboard.provider_accounts.total, 1);
    assert_eq!(dashboard.recent_requests.len(), 1);
    assert_eq!(dashboard.recent_requests[0].id, "req_observe_success");
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        2
    );

    let account_usage = repository
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(range, vec!["acct_observe".to_owned()])
                .expect("account usage query"),
        )
        .await
        .expect("provider account usage");
    assert_eq!(account_usage[0].request_count, 2);
    assert_eq!(account_usage[0].models[0].request_count, 2);
    assert_eq!(account_usage[0].cost_coverage.provider_reported_count, 1);
    assert_eq!(account_usage[0].cost_coverage.unavailable_count, 1);
    assert_eq!(account_usage[0].costs[0].amount.as_str(), "1.25");

    let usage_page = repository
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                provider_account_ref: Some("acct_observe".to_owned()),
                ..UsageRecordFilter::default()
            },
            cursor: None,
            page: ObservabilityPageNumber::new(1).expect("page"),
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("usage records");
    assert_eq!(usage_page.total, 2);

    let detail = repository
        .usage_record_detail("req_observe_failed")
        .await
        .expect("usage detail");
    assert_eq!(detail.attempts.len(), 2);
    assert_eq!(detail.attempts[0].source, "ops_event");
    assert_eq!(detail.attempts[1].source, "model_request");

    let overview = repository
        .usage_summary(range, UsageRecordFilter::default())
        .await
        .expect("usage summary");
    assert_eq!(overview.attempts.attempt_count, 3);
    assert_eq!(overview.attempts.failure_count, 2);

    let succeeded = repository
        .usage_summary(
            range,
            UsageRecordFilter {
                outcome: Some("succeeded".to_owned()),
                ..UsageRecordFilter::default()
            },
        )
        .await
        .expect("filtered usage summary");
    assert_eq!(succeeded.requests.cache_eligible_request_count, 1);
    assert_eq!(succeeded.requests.cache_hit_request_count, 1);
    assert_eq!(succeeded.requests.cache_hit_request_rate(), Some(1.0));
    assert_eq!(
        succeeded
            .requests
            .latency_percentiles
            .p50_ms
            .expect("filtered p50")
            .as_f64(),
        900.0
    );

    let diagnostics = repository
        .usage_diagnostics(
            range,
            UsageRecordFilter::default(),
            DiagnosticDimension::Account,
        )
        .await
        .expect("usage diagnostics");
    assert_eq!(diagnostics[0].name, "acct_observe");
    assert_eq!(diagnostics[0].request_count, 2);
    assert_eq!(diagnostics[0].costs[0].amount.as_str(), "1.25");

    let errors = repository
        .list_ops_errors(OpsErrorQuery {
            range,
            filter: OpsErrorFilter::default(),
            cursor: None,
            page: ObservabilityPageNumber::new(1).expect("page"),
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("ops errors");
    assert_eq!(errors.total, 2);

    database.close().await;
}

async fn seed_observability_facts(
    pool: &PgPool,
    now: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into provider_instances (
           id, provider_kind, name, base_url, enabled, created_at, updated_at
         ) values (
           'inst_observe', 'openai', 'observability', 'https://example.invalid',
           true, $1, $1
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into provider_accounts (
           id, provider_instance_id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, enabled, availability,
           availability_observed_at, created_at, updated_at
         ) values (
           'acct_observe', 'inst_observe', 'openai', 'primary', 'account@example.invalid',
           'user-observe', null, 'pro', '{}'::jsonb, 1, false, $1 + interval '1 day',
           true, 'ready', $1, $1, $1
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, input_token_estimate,
           provider_instance_id, provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           upstream_send_state, downstream_committed_at, outcome, client_status_code,
           upstream_status_code, client_response_id, upstream_response_id,
           input_tokens, output_tokens, cached_tokens, cache_write_tokens, reasoning_tokens,
           total_tokens, cost_source, cost_amount, cost_currency, first_token_ms, latency_ms,
           started_at, deadline_at, completed_at
         ) values (
           'req_observe_success', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 100, 'inst_observe', 'openai', 'acct_observe',
           'acct_observe', 'upstream-model',
           'http_sse', 1, 'sent', $1 - interval '19 minutes', 'succeeded', 200, 200,
           'resp_observe_success', 'upstream_resp_success', 100, 20, 40, 3, 5, 120,
           'provider_reported', 1.25, 'USD', 120, 900,
           $1 - interval '20 minutes', $1 + interval '10 minutes', $1 - interval '19 minutes'
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, input_token_estimate,
           provider_instance_id, provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           upstream_send_state, outcome, client_status_code, upstream_status_code,
           error_kind, provider_error_code, error_message, retry_after_ms,
           input_tokens, cached_tokens, cost_source, latency_ms,
           started_at, deadline_at, completed_at
         ) values (
           'req_observe_failed', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 80, 'inst_observe', 'openai', 'acct_observe',
           'acct_observe', 'upstream-model',
           'http_sse', 2, 'sent', 'failed', 502, 429, 'rate_limited', 'rate_limit',
           'upstream limited', 1000, 0, 0, 'unavailable', 700,
           $1 - interval '10 minutes', $1 + interval '20 minutes', $1 - interval '9 minutes'
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into ops_events (
           id, model_request_id, attempt_index, level, component, operation,
           provider_instance_id, provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, failure_kind, status_code,
           provider_error_code, retry_after_ms, latency_ms, message, occurrence_count, created_at
         ) values (
           'ops_observe_retry', 'req_observe_failed', 1, 'warning', 'routing', 'responses',
           'inst_observe', 'openai', 'acct_observe', 'acct_observe',
           'upstream-model', 'rate_limited', 429, 'rate_limit', 1000, 300,
           'first account was limited', 1, $1 - interval '9 minutes 30 seconds'
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
