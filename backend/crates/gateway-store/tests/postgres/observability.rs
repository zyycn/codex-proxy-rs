use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use chrono::{TimeDelta, Utc};
use futures::future::BoxFuture;
use gateway_admin::{
    model::{PageSize, observability as admin_observability},
    ports::store::ObservabilityStore as AdminObservabilityStore,
};
use gateway_core::{
    engine::credential::{CredentialRevision, ProviderAccountId},
    provider_ports::{
        ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope, ProviderScopedCooldown,
        ProviderStoreError,
    },
};
use gateway_store::postgres::{
    DiagnosticDimension, ObservabilityPageSize, ObservabilityRange, ObservabilityRepository,
    OpsErrorFilter, OpsErrorQuery, PgAdminObservabilityStore, PgObservabilityRepository,
    ProviderAccountUsageQuery, UsageRecordFilter, UsageRecordQuery,
};
use sqlx::PgPool;

use super::{
    TestDatabase, admin_observability_store, observability_query_budget, observability_repository,
};

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
async fn observability_preserves_and_filters_opaque_response_ids() {
    let Some(database) = TestDatabase::create("observability_opaque_response_id").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let response_id = format!("resp_{}\0opaque", "x".repeat(4_096));
    sqlx::query(
        "update model_requests
         set client_response_id = $1, upstream_response_id = $1
         where id = 'req_observe_success'",
    )
    .bind(response_id.as_bytes().to_vec())
    .execute(&database.pool)
    .await
    .expect("persist opaque response IDs");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let records = repository
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                response_id: Some(response_id.clone()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("filter opaque response ID");
    assert_eq!(records.total, 1);
    assert_eq!(records.items[0].id, "req_observe_success");
    let detail = repository
        .usage_record_detail("req_observe_success")
        .await
        .expect("usage detail with opaque response IDs");
    assert_eq!(
        detail.request.client_response_id.as_deref(),
        Some(response_id.as_str())
    );
    assert_eq!(
        detail.request.upstream_response_id.as_deref(),
        Some(response_id.as_str())
    );

    database.close().await;
}

#[tokio::test]
async fn usage_page_should_always_return_total() {
    let Some(database) = TestDatabase::create("usage_page_with_total").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    let page = observability_repository(&database.pool)
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("usage page with total");

    assert_eq!(page.total, 1);
    assert_eq!(page.current_page, 1);
    assert_eq!(page.page_size, 10);
    database.close().await;
}

#[tokio::test]
async fn usage_search_should_match_literal_prefix_instead_of_substring() {
    let Some(database) = TestDatabase::create("usage_literal_prefix_search").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    let page = observability_repository(&database.pool)
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                search: Some("observe_success".to_owned()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("usage substring search");

    assert_eq!(page.total, 0);
    database.close().await;
}

#[tokio::test]
async fn ops_search_should_treat_sql_wildcards_as_literals() {
    let Some(database) = TestDatabase::create("ops_literal_wildcard_search").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    let page = observability_repository(&database.pool)
        .list_ops_errors(OpsErrorQuery {
            range,
            filter: OpsErrorFilter {
                search: Some("req%".to_owned()),
                ..OpsErrorFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("ops literal wildcard search");

    assert_eq!(page.total, 0);
    database.close().await;
}

#[tokio::test]
async fn dashboard_account_metrics_should_partition_account_statuses() {
    let Some(database) = TestDatabase::create("dashboard_account_metrics").await else {
        return;
    };
    let now = Utc::now();
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, upstream_user_id, authentication_kind,
           provider_credentials_json, credential_revision, has_refresh_token,
           access_token_expires_at, enabled, credential_state,
           quota_access_state, quota_evidence, quota_access_observed_at,
           credential_observed_at, created_at, updated_at
         ) values
           ('acct_available', 'openai', 'available', 'user-available', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_expired', 'openai', 'expired', 'user-expired', 'oauth',
            '{}'::jsonb, 1, false, $1 - interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_rate_cooldown', 'openai', 'rate-cooldown', 'user-rate-cooldown', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_usage_limit', 'xai', 'usage-limit', 'user-usage-limit', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'usage_limit_reached', $1,
            $1, $1, $1),
           ('acct_banned', 'xai', 'banned', 'user-banned', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'banned', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_disabled', 'xai', 'disabled', 'user-disabled', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', false, 'ready', 'allowed', null, $1,
            $1, $1, $1)",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("seed account metric states");
    let repository = observability_repository(&database.pool);
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("dashboard range");

    let metrics = repository
        .dashboard_summary(range, now)
        .await
        .expect("dashboard summary")
        .provider_accounts;

    assert_eq!(
        (
            metrics.total,
            metrics.normal,
            metrics.quota_exhausted,
            metrics.rate_limited,
            metrics.disabled,
            metrics.error,
        ),
        (6, 2, 1, 0, 1, 2),
    );
    assert_eq!(
        metrics.total,
        metrics.normal
            + metrics.quota_exhausted
            + metrics.rate_limited
            + metrics.disabled
            + metrics.error
    );
    database.close().await;
}

#[tokio::test]
async fn dashboard_account_metrics_with_cooldowns_should_only_reclassify_eligible_accounts() {
    let Some(database) = TestDatabase::create("dashboard_account_metrics_cooldowns").await else {
        return;
    };
    let now = Utc::now();
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, upstream_user_id, authentication_kind,
           provider_credentials_json, credential_revision, has_refresh_token,
           access_token_expires_at, enabled, credential_state,
           quota_access_state, quota_evidence, quota_access_observed_at,
           credential_observed_at, created_at, updated_at
         ) values
           ('acct_metrics_active', 'openai', 'active', 'user-active', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_metrics_active_cooling', 'openai', 'active-cooling', 'user-active-cooling', 'oauth',
            '{}'::jsonb, 2, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_metrics_ready_quota', 'openai', 'ready-quota', 'user-ready-quota', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_cooling', 'openai', 'cooling', 'user-cooling', 'oauth',
            '{}'::jsonb, 3, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_stale_cooldown', 'openai', 'stale-cooldown', 'user-stale-cooldown', 'oauth',
            '{}'::jsonb, 4, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_metrics_persistent_quota', 'openai', 'persistent-quota', 'user-persistent-quota', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_expired', 'openai', 'expired', 'user-expired', 'oauth',
            '{}'::jsonb, 5, false, $1 - interval '1 second', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_unknown', 'openai', 'unknown', 'user-unknown', 'oauth',
            '{}'::jsonb, 6, false, $1 + interval '1 day', true, 'unknown', 'unknown', null, null,
            $1, $1, $1),
           ('acct_metrics_disabled', 'openai', 'disabled', 'user-disabled', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', false, 'ready', 'allowed', null, $1,
            $1, $1, $1)",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("seed cooldown account metric states");
    let cooldowns = StaticCooldowns::new([
        test_cooldown("acct_metrics_active_cooling", 2),
        test_cooldown("acct_metrics_cooling", 3),
        test_cooldown("acct_metrics_expired", 5),
        test_cooldown("acct_metrics_unknown", 6),
        test_cooldown("acct_metrics_stale_cooldown", 3),
    ]);
    let repository = PgObservabilityRepository::new(
        database.pool.clone(),
        Some(Arc::new(cooldowns)),
        observability_query_budget(),
    );
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("dashboard range");

    let metrics = repository
        .dashboard_summary(range, now)
        .await
        .expect("dashboard summary")
        .provider_accounts;

    assert_eq!(
        (
            metrics.total,
            metrics.normal,
            metrics.rate_limited,
            metrics.quota_exhausted,
            metrics.disabled,
            metrics.error,
        ),
        (9, 1, 2, 3, 1, 2),
    );
    assert_eq!(
        metrics.total,
        metrics.normal
            + metrics.rate_limited
            + metrics.quota_exhausted
            + metrics.disabled
            + metrics.error
    );
    database.close().await;
}

struct StaticCooldowns {
    cooldowns: BTreeMap<ProviderAccountId, ProviderCooldown>,
}

impl StaticCooldowns {
    fn new(cooldowns: impl IntoIterator<Item = ProviderCooldown>) -> Self {
        Self {
            cooldowns: cooldowns
                .into_iter()
                .map(|cooldown| (cooldown.account_id().clone(), cooldown))
                .collect(),
        }
    }
}

impl ProviderCooldownPort for StaticCooldowns {
    fn put_if_later(
        &self,
        _cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async move { Ok(self.cooldowns.get(account_id).cloned()) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn put_scoped_if_later(
        &self,
        _cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn clear_all<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
}

fn test_cooldown(account_id: &str, revision: u64) -> ProviderCooldown {
    ProviderCooldown::new(
        ProviderAccountId::new(account_id).expect("test account ID"),
        CredentialRevision::new(revision).expect("test revision"),
        SystemTime::now() + Duration::from_secs(60),
    )
}

#[tokio::test]
async fn calculated_usage_billing_facts_keep_only_completed_calculated_costs() {
    let Some(database) = TestDatabase::create("calculated_usage_billing_facts").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    seed_calculated_billing_facts(&database.pool, now)
        .await
        .expect("seed calculated billing facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let facts = repository
        .usage_calculated_billing_facts(range, UsageRecordFilter::default())
        .await
        .expect("calculated usage billing facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].provider_kind, "openai");
    assert_eq!(facts[0].upstream_model_id, "gpt-5.5");
    assert_eq!(facts[0].input_tokens, Some(800));
    assert_eq!(facts[0].output_tokens, Some(200));
    assert_eq!(facts[0].service_tier.as_deref(), Some("priority"));
    assert_eq!(facts[0].total.amount.as_str(), "1.25");

    let store = admin_observability_store(&database.pool);
    let facts = store
        .usage_calculated_billing_facts(
            admin_observability::TimeRange::new(range.start, range.end)
                .expect("admin observability range"),
            admin_observability::UsageFilter::default(),
        )
        .await
        .expect("admin calculated usage billing facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].service_tier.as_deref(), Some("priority"));
    assert_eq!(facts[0].total.amount.as_str(), "1.25");

    database.close().await;
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
    let store = admin_observability_store(&database.pool);

    let dashboard = store
        .dashboard_summary(range, now)
        .await
        .expect("admin dashboard summary");
    assert_eq!(dashboard.range, range);
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.first_token_latency_sum_ms)
            .sum::<u64>(),
        120
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.latency_sum_ms)
            .sum::<u64>(),
        900
    );
    assert_eq!(dashboard.provider_accounts.total, 1);
    assert_eq!(dashboard.account_usage[0].request_count, 1);
    assert_eq!(dashboard.account_usage[0].request_buckets.len(), 2);
    assert_eq!(
        dashboard.account_usage[0]
            .request_buckets
            .iter()
            .map(|bucket| bucket.request_count)
            .sum::<u64>(),
        1,
    );
    assert_eq!(dashboard.recent_requests.len(), 1);
    assert_eq!(
        dashboard.recent_requests[0].service_tier.as_deref(),
        Some("priority")
    );
    assert_eq!(dashboard.recent_requests[0].id, "req_observe_success");
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
        3,
    );
    assert_eq!(
        dashboard_trend
            .iter()
            .map(|point| point.metrics.failure_count)
            .sum::<u64>(),
        1,
    );
    assert!(
        dashboard
            .trend
            .iter()
            .chain(&dashboard_trend)
            .all(|point| point.costs.is_empty()),
        "Dashboard 趋势不应触发未展示的成本聚合",
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
        2,
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
            current_page: 1,
            page_size: PageSize::new(1).expect("page size"),
        })
        .await
        .expect("first usage page");
    assert_eq!(first_page.total, 1);
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.current_page, 1);
    assert_eq!(first_page.page_size, 1);
    assert_eq!(
        first_page.items[0].service_tier.as_deref(),
        Some("priority")
    );

    let deep_page = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter::default(),
            current_page: 129,
            page_size: PageSize::new(1).expect("page size"),
        })
        .await
        .expect("direct deep usage page");
    assert_eq!(deep_page.current_page, 129);
    assert_eq!(deep_page.page_size, 1);
    assert_eq!(deep_page.total, 1);
    assert!(deep_page.items.is_empty());

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
            current_page: 1,
            page_size: PageSize::new(10).expect("page size"),
        })
        .await
        .expect("fully filtered usage page");
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.items[0].id, "req_observe_success");
    assert_eq!(filtered.items[0].service_tier.as_deref(), Some("priority"));

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
            current_page: 1,
            page_size: PageSize::new(10).expect("page size"),
        })
        .await
        .expect("other outcome filter should reach PostgreSQL");
    assert_eq!(other_outcome.total, 0);
    assert!(other_outcome.items.is_empty());

    let detail = store
        .usage_record_detail("req_observe_success")
        .await
        .expect("admin usage detail");
    assert_eq!(
        detail.request.outcome,
        admin_observability::RequestOutcome::Succeeded
    );
    assert_eq!(detail.request.service_tier.as_deref(), Some("priority"));
    assert_eq!(detail.request.routing_scope, "groups");
    assert_eq!(detail.request.routing_group_refs, ["grp_history"]);
    assert_eq!(
        detail.request.routing_group_names_snapshot,
        ["Historical group"]
    );
    assert_eq!(
        detail.request.upstream_request_id.as_deref(),
        Some("upstream_req_success")
    );
    assert_eq!(detail.attempts.len(), 1);
    assert_eq!(
        detail.attempts[0].outcome,
        admin_observability::RequestOutcome::Succeeded
    );
    assert!(
        store
            .usage_record_detail("req_observe_failed")
            .await
            .is_err()
    );
    assert!(
        store
            .usage_record_detail("req_observe_uncommitted")
            .await
            .is_err()
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
    assert_eq!(diagnostics[0].key, "acct_observe");
    assert_eq!(diagnostics[0].name, "account@example.invalid");
    assert_eq!(diagnostics[0].cost_coverage.provider_reported_count, 1);
    assert_eq!(diagnostics[0].costs[0].amount.as_str(), "1.25");

    let errors = store
        .list_ops_errors(admin_observability::OpsErrorQuery {
            range,
            filter: admin_observability::OpsErrorFilter {
                request_id: Some("req_observe_failed".to_owned()),
                provider_kind: Some("openai".to_owned()),
                provider_account_ref: Some("acct_observe".to_owned()),
                operation: Some("responses".to_owned()),
                endpoint: Some("/v1/responses".to_owned()),
                model: Some("upstream-model".to_owned()),
                failure_kind: Some("rate_limited".to_owned()),
                status_code: Some(429),
                search: Some("req_observe_failed".to_owned()),
                ..admin_observability::OpsErrorFilter::default()
            },
            current_page: 1,
            page_size: PageSize::new(10).expect("page size"),
        })
        .await
        .expect("admin ops errors");
    assert_eq!(errors.total, 2);
    assert!(errors.items.iter().all(|item| item.occurred_at <= now));

    let deep_errors = store
        .list_ops_errors(admin_observability::OpsErrorQuery {
            range,
            filter: admin_observability::OpsErrorFilter::default(),
            current_page: 129,
            page_size: PageSize::new(1).expect("page size"),
        })
        .await
        .expect("direct deep ops page");
    assert_eq!(deep_errors.current_page, 129);
    assert_eq!(deep_errors.page_size, 1);
    assert_eq!(deep_errors.total, 2);
    assert!(deep_errors.items.is_empty());

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
                current_page: 1,
                page_size: PageSize::new(10).expect("page size"),
            })
            .await
            .expect("fully forwarded ops filter");
        assert_eq!(page.total, 0);
    }

    database.close().await;
}

#[tokio::test]
async fn dashboard_summary_totals_include_history_outside_selected_range() {
    let Some(database) = TestDatabase::create("observability_dashboard_totals").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    sqlx::query(
        "update model_requests
         set started_at = $1 - interval '2 hours'
         where id = 'req_observe_success'",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("move historical request outside selected range");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let dashboard = repository
        .dashboard_summary(range, now)
        .await
        .expect("dashboard summary");
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        2
    );
    assert!(dashboard.account_usage.is_empty());

    let explicit_account_usage = repository
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(range, vec!["acct_observe".to_owned()])
                .expect("account usage query"),
        )
        .await
        .expect("provider account usage");
    assert_eq!(explicit_account_usage.len(), 1);
    assert_eq!(explicit_account_usage[0].request_count, 0);

    assert_eq!(
        (
            dashboard.totals.request_count,
            dashboard.totals.input_tokens,
            dashboard.totals.cached_tokens,
            dashboard.totals.total_tokens,
        ),
        (3, 100, 40, 120),
    );
    assert_eq!(
        dashboard
            .totals
            .billing_usd
            .as_ref()
            .map(|amount| amount.as_str()),
        Some("1.25"),
    );

    database.close().await;
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
    let repository = observability_repository(&database.pool);

    let dashboard = repository
        .dashboard_summary(range, now)
        .await
        .expect("dashboard summary");
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.success_count)
            .sum::<u64>(),
        2
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.failure_count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.caller_error_count)
            .sum::<u64>(),
        0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.cache_eligible_request_count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.cache_hit_request_count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.latency_percentiles.p50_ms)
            .expect("latency p50")
            .as_f64(),
        900.0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.latency_percentiles.p95_ms)
            .expect("latency p95")
            .as_f64(),
        900.0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.latency_percentiles.p99_ms)
            .expect("latency p99")
            .as_f64(),
        900.0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.first_token_latency_percentiles.p50_ms)
            .expect("first token p50")
            .as_f64(),
        120.0
    );
    assert_eq!(dashboard.provider_accounts.total, 1);
    assert_eq!(dashboard.account_usage[0].request_count, 1);
    assert_eq!(dashboard.account_usage[0].request_buckets.len(), 2);
    assert_eq!(
        dashboard.account_usage[0]
            .request_buckets
            .iter()
            .map(|bucket| bucket.request_count)
            .sum::<u64>(),
        1,
    );
    assert_eq!(dashboard.recent_requests.len(), 1);
    assert_eq!(dashboard.recent_requests[0].id, "req_observe_success");
    assert_eq!(
        dashboard.recent_requests[0].service_tier.as_deref(),
        Some("priority")
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.failure_count)
            .sum::<u64>(),
        1
    );

    let account_usage = repository
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(range, vec!["acct_observe".to_owned()])
                .expect("account usage query")
                .with_hourly_request_buckets()
                .expect("account request timeline"),
        )
        .await
        .expect("provider account usage");
    assert_eq!(account_usage[0].request_count, 1);
    assert_eq!(account_usage[0].authentication_kind, "oauth");
    assert_eq!(account_usage[0].models[0].request_count, 1);
    assert_eq!(account_usage[0].cost_coverage.provider_reported_count, 1);
    assert_eq!(account_usage[0].cost_coverage.unavailable_count, 0);
    assert_eq!(account_usage[0].costs[0].amount.as_str(), "1.25");
    assert_eq!(
        account_usage[0]
            .request_buckets
            .iter()
            .map(|bucket| bucket.request_count)
            .collect::<Vec<_>>(),
        vec![1, 0],
    );
    assert_eq!(
        (
            account_usage[0].image_input_tokens,
            account_usage[0].image_output_tokens,
            account_usage[0].image_request_count,
            account_usage[0].image_request_failed_count,
            account_usage[0].models[0].image_request_count,
            account_usage[0].models[0].image_request_failed_count,
        ),
        (Some(31), Some(9), 1, 0, 1, 0)
    );

    let usage_page = repository
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                provider_account_ref: Some("acct_observe".to_owned()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("usage records");
    assert_eq!(usage_page.total, 1);
    let successful_image = usage_page
        .items
        .iter()
        .find(|record| record.id == "req_observe_success")
        .expect("successful image usage record");
    assert_eq!(
        (
            successful_image.service_tier.as_deref(),
            successful_image
                .provider_account_authentication_kind
                .as_deref(),
            successful_image.image_input_tokens,
            successful_image.image_output_tokens,
        ),
        (Some("priority"), Some("oauth"), Some(31), Some(9))
    );

    let successful_detail = repository
        .usage_record_detail("req_observe_success")
        .await
        .expect("successful usage detail");
    assert_eq!(
        (
            successful_detail.request.websocket_pool.as_deref(),
            successful_detail.request.image_generation_requested,
            successful_detail.request.image_generation_succeeded,
        ),
        (Some("reuse"), true, Some(true))
    );

    assert!(
        repository
            .usage_record_detail("req_observe_failed")
            .await
            .is_err()
    );

    let overview = repository
        .usage_summary(range, UsageRecordFilter::default())
        .await
        .expect("usage summary");
    assert_eq!(overview.attempts.attempt_count, 4);
    assert_eq!(overview.attempts.failure_count, 2);
    assert_eq!(overview.requests.request_count, 3);
    assert_eq!(overview.requests.failure_count, 1);
    assert_eq!(
        (
            overview.providers[0].request_count,
            overview.providers[0].attempt_count,
            overview.providers[0].failure_count,
            overview.providers[0].total_tokens,
        ),
        (3, 4, 1, 120),
    );

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
    assert_eq!(diagnostics[0].key, "acct_observe");
    assert_eq!(diagnostics[0].name, "account@example.invalid");
    assert_eq!(diagnostics[0].request_count, 3);
    assert_eq!(diagnostics[0].success_count, 2);
    assert_eq!(diagnostics[0].failure_count, 1);
    assert_eq!(diagnostics[0].retry_count, 1);
    assert_eq!(diagnostics[0].costs[0].amount.as_str(), "1.25");

    let errors = repository
        .list_ops_errors(OpsErrorQuery {
            range,
            filter: OpsErrorFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("ops errors");
    assert_eq!(errors.total, 2);
    let request_error = errors
        .items
        .iter()
        .find(|error| error.source == "model_request")
        .expect("request error");
    assert_eq!(request_error.client_status_code, Some(502));
    assert_eq!(request_error.upstream_status_code, Some(429));
    assert_eq!(request_error.client_ip.as_deref(), Some("203.0.113.9"));
    assert_eq!(
        request_error.user_agent.as_deref(),
        Some("codex-cli/0.144.0")
    );
    assert_eq!(
        request_error.requested_model_id.as_deref(),
        Some("public-model")
    );
    let attempt_error = errors
        .items
        .iter()
        .find(|error| error.source == "ops_event")
        .expect("attempt error");
    assert_eq!(attempt_error.client_status_code, None);
    assert_eq!(attempt_error.upstream_status_code, Some(429));
    assert_eq!(attempt_error.endpoint.as_deref(), Some("/v1/responses"));
    assert_eq!(attempt_error.client_ip.as_deref(), Some("203.0.113.9"));
    assert_eq!(
        attempt_error.user_agent.as_deref(),
        Some("codex-cli/0.144.0")
    );

    database.close().await;
}

async fn seed_observability_facts(
    pool: &PgPool,
    now: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind,
           provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, enabled, credential_state,
           credential_observed_at, created_at, updated_at
         ) values (
           'acct_observe', 'openai', 'primary', 'account@example.invalid',
           'user-observe', null, 'pro', 'oauth', '{}'::jsonb, 1, false, $1 + interval '1 day',
           true, 'ready', $1, $1, $1
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id,
           provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport,
           provider_account_name_snapshot, provider_account_email_snapshot,
           provider_account_authentication_kind_snapshot,
           attempt_count, upstream_send_state, outcome, client_status_code,
           input_tokens, output_tokens, total_tokens, cost_source, latency_ms,
           started_at, deadline_at, completed_at,
           routing_scope, routing_group_refs, routing_group_names_snapshot
         ) values (
           'req_observe_uncommitted', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 'openai', 'acct_observe',
           'acct_observe', 'upstream-model', 'http_sse',
           'primary', 'account@example.invalid', 'oauth',
           1, 'sent', 'succeeded', 200,
           900, 900, 1800, 'unavailable', 650,
           $1 - interval '15 minutes', $1 + interval '10 minutes', $1 - interval '14 minutes',
           'all', '{}'::text[], '[]'::jsonb
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id,
           provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, websocket_pool,
           service_tier,
           provider_account_name_snapshot, provider_account_email_snapshot,
           provider_account_authentication_kind_snapshot,
           attempt_count,
           upstream_send_state, downstream_committed_at, outcome, client_status_code,
           upstream_status_code, client_response_id, upstream_request_id, upstream_response_id,
           input_tokens, output_tokens, cached_tokens, cache_write_tokens, reasoning_tokens,
           image_input_tokens, image_output_tokens, total_tokens,
           image_generation_requested, image_generation_succeeded,
           cost_source, cost_amount, cost_currency, first_token_ms, latency_ms,
           started_at, deadline_at, completed_at,
           routing_scope, routing_group_refs, routing_group_names_snapshot
         ) values (
           'req_observe_success', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 'openai', 'acct_observe',
           'acct_observe', 'upstream-model',
           'http_sse', 'reuse', 'priority',
           'primary', 'account@example.invalid', 'oauth',
           1, 'sent', $1 - interval '19 minutes', 'succeeded', 200, 200,
           'resp_observe_success', 'upstream_req_success', 'upstream_resp_success',
           100, 20, 40, 3, 5, 31, 9, 120, true, true,
           'provider_reported', 1.25, 'USD', 120, 900,
           $1 - interval '20 minutes', $1 + interval '10 minutes', $1 - interval '19 minutes',
           'groups', array['grp_history'], jsonb_build_array('Historical group')
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, service_tier,
           provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           provider_account_name_snapshot, provider_account_email_snapshot,
           provider_account_authentication_kind_snapshot,
           upstream_send_state, outcome, client_status_code, upstream_status_code,
           error_kind, provider_error_code, error_message, retry_after_ms,
           input_tokens, cached_tokens, image_generation_requested,
           image_generation_succeeded, cost_source, latency_ms,
           client_ip, user_agent, reasoning_effort, reasoning_preset,
           request_kind, subagent_kind, compact,
           started_at, deadline_at, completed_at,
           routing_scope, routing_group_refs, routing_group_names_snapshot
         ) values (
           'req_observe_failed', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 'priority', 'openai', 'acct_observe',
           'acct_observe', 'upstream-model',
           'http_sse', 2,
           'primary', 'account@example.invalid', 'oauth',
           'sent', 'failed', 502, 429, 'rate_limited', 'rate_limit',
           'upstream limited', 1000, 0, 0, true, false, 'unavailable', 700,
           '203.0.113.9', 'codex-cli/0.144.0', 'medium', null,
           'root', null, false,
           $1 - interval '10 minutes', $1 + interval '20 minutes', $1 - interval '9 minutes',
           'all', '{}'::text[], '[]'::jsonb
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into ops_events (
           id, model_request_id, attempt_index, level, component, operation,
           provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, failure_kind, status_code,
           provider_account_name_snapshot, provider_account_email_snapshot,
           provider_account_authentication_kind_snapshot,
           provider_error_code, retry_after_ms, latency_ms, message, occurrence_count, created_at
         ) values (
           'ops_observe_retry', 'req_observe_failed', 1, 'warning', 'routing', 'responses',
           'openai', 'acct_observe', 'acct_observe',
           'upstream-model', 'rate_limited', 429,
           'primary', 'account@example.invalid', 'oauth',
           'rate_limit', 1000, 300,
           'first account was limited', 1, $1 - interval '9 minutes 30 seconds'
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_calculated_billing_facts(
    pool: &PgPool,
    now: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           provider_account_name_snapshot, provider_account_email_snapshot,
           provider_account_authentication_kind_snapshot,
           upstream_send_state, downstream_committed_at, outcome, client_status_code,
           input_tokens, output_tokens, cached_tokens, cache_write_tokens, total_tokens, service_tier,
           cost_source, cost_amount, cost_currency, started_at, deadline_at, completed_at,
           routing_scope, routing_group_refs, routing_group_names_snapshot
         ) values (
           'req_observe_calculated', 'key_observe', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'public-model', 'openai', 'acct_observe', 'acct_observe', 'gpt-5.5',
           'http_sse', 1,
           'primary', 'account@example.invalid', 'oauth',
           'sent', $1 - interval '29 minutes', 'succeeded', 200,
           800, 200, 0, 0, 1000, 'priority', 'calculated', 1.25, 'USD',
           $1 - interval '30 minutes', $1 + interval '10 minutes', $1 - interval '29 minutes',
           'all', '{}'::text[], '[]'::jsonb
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           provider_account_name_snapshot, provider_account_email_snapshot,
           provider_account_authentication_kind_snapshot,
           upstream_send_state, outcome, client_status_code, input_tokens, output_tokens,
           total_tokens, cost_source, cost_amount, cost_currency, started_at, deadline_at,
           completed_at, routing_scope, routing_group_refs, routing_group_names_snapshot
         ) values (
           'req_observe_calculated_uncommitted', 'key_observe', 1, 'openai', 'responses',
           '/v1/responses', 'http_sse', 'public-model', 'openai', 'acct_observe',
           'acct_observe', 'gpt-5.5', 'http_sse', 1,
           'primary', 'account@example.invalid', 'oauth',
           'sent', 'succeeded', 200, 800, 200,
           1000, 'calculated', 1.25, 'USD',
           $1 - interval '40 minutes', $1 + interval '10 minutes', $1 - interval '39 minutes',
           'all', '{}'::text[], '[]'::jsonb
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
