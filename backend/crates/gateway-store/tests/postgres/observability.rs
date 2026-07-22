use std::num::NonZeroU32;

use chrono::{TimeDelta, Utc};
use gateway_admin::{
    model::{PageSize, observability as admin_observability},
    ports::store::ObservabilityStore as AdminObservabilityStore,
};
use gateway_store::postgres::{
    DiagnosticDimension, ObservabilityPageNumber, ObservabilityPageSize, ObservabilityRange,
    ObservabilityRepository, OpsErrorFilter, OpsErrorQuery, PgAdminObservabilityStore,
    PgObservabilityRepository, ProviderAccountUsageQuery, UsageRecordFilter, UsageRecordQuery,
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
fn usage_outcome_filter_should_accept_bounded_unknown_values() {
    assert!(
        UsageRecordFilter {
            outcome: Some("provider_future_state".to_owned()),
            ..UsageRecordFilter::default()
        }
        .validate()
        .is_ok()
    );
    assert!(
        UsageRecordFilter {
            outcome: Some("a".repeat(257)),
            ..UsageRecordFilter::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn postgres_observability_adapter_implements_query_port() {
    fn assert_port<T: ObservabilityRepository>() {}
    assert_port::<PgObservabilityRepository>();
}

#[test]
fn postgres_admin_observability_adapter_implements_terminal_port() {
    fn assert_port<T: AdminObservabilityStore>() {}
    assert_port::<PgAdminObservabilityStore>();
}

#[tokio::test]
async fn admin_observability_adapter_preserves_utc_queries_metrics_costs_and_details() {
    let Some(database) = TestDatabase::create("admin_observability").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range =
        admin_observability::TimeRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
            .expect("admin observability range");
    let store = PgAdminObservabilityStore::new(database.pool.clone());

    let dashboard = store
        .dashboard_summary(range)
        .await
        .expect("admin dashboard summary");
    assert_eq!(dashboard.range, range);
    assert_eq!(dashboard.requests.request_count, 2);
    assert_eq!(dashboard.requests.first_token_latency_sum_ms, 120);
    assert_eq!(dashboard.requests.latency_sum_ms, 1_600);
    assert_eq!(dashboard.requests.min_latency_ms, Some(700));
    assert_eq!(dashboard.requests.max_latency_ms, Some(900));
    assert_eq!(dashboard.attempts.attempt_count, 3);
    assert_eq!(dashboard.attempts.cost_coverage.provider_reported_count, 1);
    assert_eq!(dashboard.attempts.cost_coverage.unavailable_count, 1);
    assert_eq!(dashboard.attempts.costs[0].amount.as_str(), "1.25");
    assert_eq!(dashboard.provider_accounts.total, 1);
    assert_eq!(dashboard.recent_requests.len(), 1);
    assert_eq!(
        dashboard.recent_requests[0].outcome,
        admin_observability::RequestOutcome::Succeeded,
    );
    assert_eq!(
        dashboard.recent_requests[0]
            .cost_amount
            .as_ref()
            .expect("dashboard request cost")
            .as_str(),
        "1.25",
    );

    let dashboard_trend = store
        .dashboard_trend(range)
        .await
        .expect("admin dashboard trend");
    assert_eq!(
        dashboard_trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        2,
    );

    let trend = store
        .usage_trend(
            range,
            admin_observability::UsageFilter {
                outcome: Some(admin_observability::RequestOutcome::Succeeded),
                ..admin_observability::UsageFilter::default()
            },
        )
        .await
        .expect("admin usage trend");
    assert_eq!(
        trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        1,
    );
    assert_eq!(
        trend
            .iter()
            .flat_map(|point| &point.costs)
            .next()
            .expect("trend cost")
            .amount
            .as_str(),
        "1.25",
    );

    let first_page = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter::default(),
            cursor: None,
            page: admin_page(1),
            page_size: PageSize::new(1).expect("page size"),
        })
        .await
        .expect("first usage page");
    assert_eq!(first_page.total, 2);
    assert_eq!(first_page.items.len(), 1);
    let second_page = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter::default(),
            cursor: first_page.next_cursor,
            page: admin_page(1),
            page_size: PageSize::new(1).expect("page size"),
        })
        .await
        .expect("second usage page");
    assert_eq!(second_page.items.len(), 1);
    assert_ne!(first_page.items[0].id, second_page.items[0].id);

    let filtered = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter {
                client_api_key_ref: Some("key_observe".to_owned()),
                request_id: Some("req_observe_success".to_owned()),
                provider_account_ref: Some("acct_observe".to_owned()),
                operation: Some("responses".to_owned()),
                provider_kind: Some("openai".to_owned()),
                model: Some("upstream-model".to_owned()),
                outcome: Some(admin_observability::RequestOutcome::Succeeded),
                status_code: Some(200),
                transport: Some("http_sse".to_owned()),
                attempt_index: Some(1),
                response_id: Some("resp_observe_success".to_owned()),
                upstream_request_id: Some("upstream_req_success".to_owned()),
                search: Some("req_observe_success".to_owned()),
            },
            cursor: None,
            page: admin_page(1),
            page_size: PageSize::new(10).expect("page size"),
        })
        .await
        .expect("fully filtered usage page");
    assert_eq!(filtered.total, 1);
    assert_eq!(
        filtered.items[0].upstream_request_id.as_deref(),
        Some("upstream_req_success")
    );

    let other_outcome = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter {
                outcome: Some(
                    admin_observability::RequestOutcome::new("provider_future_state")
                        .expect("bounded other outcome"),
                ),
                ..admin_observability::UsageFilter::default()
            },
            cursor: None,
            page: admin_page(1),
            page_size: PageSize::new(10).expect("page size"),
        })
        .await
        .expect("other outcome filter should reach PostgreSQL");
    assert_eq!(other_outcome.total, 0);
    assert!(other_outcome.items.is_empty());

    let detail = store
        .usage_record_detail("req_observe_failed")
        .await
        .expect("admin usage detail");
    assert_eq!(
        detail.request.outcome,
        admin_observability::RequestOutcome::Failed
    );
    assert_eq!(detail.attempts.len(), 2);
    assert_eq!(
        detail.attempts[0].outcome,
        admin_observability::RequestOutcome::Failed
    );
    assert_eq!(
        detail.attempts[1].outcome,
        admin_observability::RequestOutcome::Failed
    );

    let overview = store
        .usage_summary(range, admin_observability::UsageFilter::default())
        .await
        .expect("admin usage overview");
    assert_eq!(overview.range, range);
    assert_eq!(overview.providers[0].provider_kind, "openai");
    assert_eq!(overview.attempts.costs[0].amount.as_str(), "1.25");

    let diagnostics = store
        .usage_diagnostics(
            range,
            admin_observability::UsageFilter::default(),
            admin_observability::DiagnosticDimension::Account,
        )
        .await
        .expect("admin diagnostics");
    assert_eq!(diagnostics[0].name, "acct_observe");
    assert_eq!(diagnostics[0].cost_coverage.provider_reported_count, 1);
    assert_eq!(diagnostics[0].costs[0].amount.as_str(), "1.25");

    let errors = store
        .list_ops_errors(admin_observability::OpsErrorQuery {
            range,
            filter: admin_observability::OpsErrorFilter {
                request_id: Some("req_observe_failed".to_owned()),
                provider_kind: Some("openai".to_owned()),
                provider_account_ref: Some("acct_observe".to_owned()),
                model: Some("upstream-model".to_owned()),
                failure_kind: Some("rate_limited".to_owned()),
                status_code: Some(429),
                search: Some("limited".to_owned()),
                ..admin_observability::OpsErrorFilter::default()
            },
            cursor: None,
            page: admin_page(1),
            page_size: PageSize::new(10).expect("page size"),
        })
        .await
        .expect("admin ops errors");
    assert_eq!(errors.total, 2);
    assert!(errors.items.iter().all(|item| item.occurred_at <= now));

    for filter in [
        admin_observability::OpsErrorFilter {
            client_api_key_ref: Some("missing-key".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            operation: Some("missing-operation".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            transport: Some("missing-transport".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            attempt_index: Some(99),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            response_id: Some("missing-response".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            upstream_request_id: Some("missing-upstream-request".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
    ] {
        let page = store
            .list_ops_errors(admin_observability::OpsErrorQuery {
                range,
                filter,
                cursor: None,
                page: admin_page(1),
                page_size: PageSize::new(10).expect("page size"),
            })
            .await
            .expect("fully forwarded ops filter");
        assert_eq!(page.total, 0);
    }

    database.close().await;
}

fn admin_page(value: u32) -> admin_observability::PageNumber {
    admin_observability::PageNumber::new(NonZeroU32::new(value).expect("positive page"))
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
    assert_eq!(
        (
            account_usage[0].image_input_tokens,
            account_usage[0].image_output_tokens,
            account_usage[0].image_request_count,
            account_usage[0].image_request_failed_count,
            account_usage[0].models[0].image_request_count,
            account_usage[0].models[0].image_request_failed_count,
        ),
        (Some(31), Some(9), 1, 1, 1, 1)
    );

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
    let successful_image = usage_page
        .items
        .iter()
        .find(|record| record.id == "req_observe_success")
        .expect("successful image usage record");
    assert_eq!(
        (
            successful_image.websocket_pool.as_deref(),
            successful_image.image_input_tokens,
            successful_image.image_output_tokens,
            successful_image.image_generation_requested,
            successful_image.image_generation_succeeded,
        ),
        (Some("reuse"), Some(31), Some(9), true, Some(true))
    );

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
           provider_account_ref, upstream_model_id, upstream_transport, websocket_pool,
           attempt_count,
           upstream_send_state, downstream_committed_at, outcome, client_status_code,
           upstream_status_code, client_response_id, upstream_request_id, upstream_response_id,
           input_tokens, output_tokens, cached_tokens, cache_write_tokens, reasoning_tokens,
           image_input_tokens, image_output_tokens, total_tokens,
           image_generation_requested, image_generation_succeeded,
           cost_source, cost_amount, cost_currency, first_token_ms, latency_ms,
           started_at, deadline_at, completed_at
         ) values (
           'req_observe_success', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 100, 'inst_observe', 'openai', 'acct_observe',
           'acct_observe', 'upstream-model',
           'http_sse', 'reuse', 1, 'sent', $1 - interval '19 minutes', 'succeeded', 200, 200,
           'resp_observe_success', 'upstream_req_success', 'upstream_resp_success',
           100, 20, 40, 3, 5, 31, 9, 120, true, true,
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
           input_tokens, cached_tokens, image_generation_requested,
           image_generation_succeeded, cost_source, latency_ms,
           started_at, deadline_at, completed_at
         ) values (
           'req_observe_failed', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 80, 'inst_observe', 'openai', 'acct_observe',
           'acct_observe', 'upstream-model',
           'http_sse', 2, 'sent', 'failed', 502, 429, 'rate_limited', 'rate_limit',
           'upstream limited', 1000, 0, 0, true, false, 'unavailable', 700,
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
