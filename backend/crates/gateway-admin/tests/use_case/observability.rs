use std::{
    num::NonZeroU32,
    str::FromStr as _,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Timelike as _, Utc};

use gateway_admin::{
    AdminServices,
    model::{
        MutationContext, PageSize, Revision,
        observability::{
            AccountPoolMetrics, AttemptMetrics, CostCoverage, CurrencyCost, DashboardObservation,
            DashboardRuntimeSlots, DiagnosticDimension, DiagnosticObservation, Granularity,
            HealthStatus, OpsErrorPage, OpsErrorQuery, PageNumber, RequestMetricPoint,
            RequestMetrics, RequestOutcome, TimeRange, TrendKind, UsageBilling,
            UsageCalculatedBillingFact, UsageDetail, UsageFilter, UsageOverview, UsagePage,
            UsageQuery, UsageRecord, china_day_start,
        },
        settings::{
            AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RotationStrategy,
            RuntimeSettings,
        },
    },
    ports::store::{AdminStoreResult, ObservabilityStore, SettingsStore},
};

#[test]
fn external_observability_range_accepts_exactly_366_days() {
    let end = Utc::now();
    let range = TimeRange::new(end - Duration::days(366), end)
        .expect("366-day external range should be accepted");
    assert_eq!(range.end, end);
}

#[test]
fn external_observability_range_rejects_over_366_days_and_reversed_range() {
    let end = Utc::now();
    assert!(TimeRange::new(end - Duration::days(367), end).is_err());
    assert!(TimeRange::new(end, end).is_err());
    assert!(TimeRange::new(end + Duration::seconds(1), end).is_err());
}

#[tokio::test]
async fn health_timeline_should_keep_exactly_china_day_quarter_hour_slots() {
    let now = Utc::now();
    let day_start = china_day_start(now);
    let current_slot = quarter_hour_start(now);
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_trend(vec![
        health_metric_point(
            day_start - Duration::minutes(15),
            RequestMetrics {
                success_count: 100,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            current_slot,
            RequestMetrics {
                success_count: 2,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            current_slot + Duration::minutes(15),
            RequestMetrics {
                success_count: 100,
                ..RequestMetrics::default()
            },
        ),
    ]);
    let services = observability_services(store).await;

    let timeline = services
        .observability()
        .dashboard_summary(observation_range(now), TrendKind::Usage)
        .await
        .expect("dashboard summary")
        .health_timeline;

    assert_eq!(timeline.points.len(), 96);
    assert_eq!(
        timeline.points.first().map(|point| point.bucket_start),
        Some(day_start)
    );
    assert_eq!(
        timeline.points.last().map(|point| point.bucket_start),
        Some(day_start + Duration::minutes(95 * 15))
    );
    assert_eq!(timeline.success_requests, 2);
    assert_eq!(
        timeline
            .points
            .iter()
            .find(|point| point.bucket_start == current_slot)
            .map(|point| point.status),
        Some(HealthStatus::LowSample)
    );
    assert!(timeline.points.iter().all(|point| {
        point.bucket_start <= quarter_hour_start(Utc::now())
            || (point.status == HealthStatus::Future && point.success_requests == 0)
    }));
}

#[tokio::test]
async fn health_timeline_should_match_legacy_status_precedence_and_thresholds() {
    let now = Utc::now();
    let current_slot = quarter_hour_start(now);
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    let services = observability_services(store.clone()).await;
    let fixtures = [
        (
            RequestMetrics {
                failure_count: 1,
                cancelled_count: 1,
                incomplete_count: 1,
                caller_error_count: 1,
                ..RequestMetrics::default()
            },
            HealthStatus::NoData,
            None,
        ),
        (
            RequestMetrics {
                failure_count: 3,
                ..RequestMetrics::default()
            },
            HealthStatus::Unavailable,
            Some(0.0),
        ),
        (
            RequestMetrics {
                success_count: 1,
                failure_count: 1,
                ..RequestMetrics::default()
            },
            HealthStatus::LowSample,
            Some(50.0),
        ),
        (
            RequestMetrics {
                success_count: 98,
                failure_count: 2,
                ..RequestMetrics::default()
            },
            HealthStatus::Unstable,
            Some(98.0),
        ),
        (
            RequestMetrics {
                success_count: 99,
                failure_count: 1,
                ..RequestMetrics::default()
            },
            HealthStatus::Stable,
            Some(99.0),
        ),
    ];

    for (metrics, expected_status, expected_reliability) in fixtures {
        store.replace_trend(vec![health_metric_point(current_slot, metrics)]);
        let timeline = services
            .observability()
            .dashboard_summary(observation_range(now), TrendKind::Usage)
            .await
            .expect("dashboard summary")
            .health_timeline;
        let point = timeline
            .points
            .iter()
            .find(|point| point.bucket_start == current_slot)
            .expect("current health slot");
        assert_eq!(point.status, expected_status);
        assert_eq!(point.reliability_percent, expected_reliability);
    }
}

#[tokio::test]
async fn dashboard_summary_should_project_rebuildable_runtime_slots() {
    let now = Utc::now();
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_runtime_slots(Some(DashboardRuntimeSlots {
        active_accounts: 3,
        used_slots: Some(2),
    }));
    let services = observability_services(store).await;

    let capacity = services
        .observability()
        .dashboard_summary(observation_range(now), TrendKind::Usage)
        .await
        .expect("dashboard summary")
        .capacity;

    assert_eq!(capacity.total_slots, 3);
    assert_eq!(capacity.used_slots, Some(2));
    assert_eq!(capacity.available_slots, Some(1));
}

#[tokio::test]
async fn observability_services_should_calculate_usage_insights_and_diagnostic_shares() {
    let now = Utc::now();
    let range = observation_range(now);
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let metrics = RequestMetrics {
        request_count: 10,
        success_count: 6,
        failure_count: 4,
        caller_error_count: 2,
        input_tokens: 800,
        output_tokens: 200,
        total_tokens: 1_000,
        latency_count: 5,
        ..RequestMetrics::default()
    };
    store.replace_overview(UsageOverview {
        range,
        requests: metrics.clone(),
        attempts: AttemptMetrics {
            attempt_count: 12,
            cost_coverage: CostCoverage {
                calculated_count: 10,
                ..CostCoverage::default()
            },
            costs: vec![CurrencyCost {
                currency: "USD".to_owned(),
                amount: gateway_admin::model::observability::DecimalAmount::from_str("1.25")
                    .expect("USD cost"),
            }],
            ..AttemptMetrics::default()
        },
        providers: Vec::new(),
    });
    store.replace_trend(vec![RequestMetricPoint {
        bucket_start: quarter_hour_start(now),
        granularity: Granularity::FifteenMinutes,
        metrics,
        cost_coverage: CostCoverage::default(),
        costs: vec![CurrencyCost {
            currency: "USD".to_owned(),
            amount: gateway_admin::model::observability::DecimalAmount::from_str("0.25")
                .expect("bucket USD cost"),
        }],
    }]);
    store.replace_calculated_billing_facts(vec![UsageCalculatedBillingFact {
        bucket_start: quarter_hour_start(now),
        provider_kind: "openai".to_owned(),
        upstream_model_id: "gpt-5.5".to_owned(),
        service_tier: None,
        input_tokens: Some(800),
        output_tokens: Some(200),
        cached_tokens: Some(0),
        cache_write_tokens: Some(0),
        total: CurrencyCost {
            currency: "USD".to_owned(),
            amount: gateway_admin::model::observability::DecimalAmount::from_str("1.25")
                .expect("calculated total"),
        },
    }]);
    store.replace_diagnostics(vec![diagnostic("openai", 3), diagnostic("xai", 1)]);
    let services = observability_services_with_calculated_billing(store).await;

    let insights = services
        .observability()
        .usage_insights(range, UsageFilter::default())
        .await
        .expect("usage insights");
    let diagnostics = services
        .observability()
        .diagnostics(range, UsageFilter::default(), DiagnosticDimension::Provider)
        .await
        .expect("usage diagnostics");

    assert_eq!(insights.health.total_requests, 8);
    assert_eq!(insights.health.failed_requests, 2);
    assert_eq!(insights.granularity, Granularity::FifteenMinutes);
    assert_eq!(insights.performance.latency_coverage, 0.5);
    assert_eq!(insights.cost.tokens_per_request, 100.0);
    assert_eq!(
        insights
            .cost
            .estimated_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("1.25")
    );
    assert_eq!(
        insights
            .cost
            .cost_per_request
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("0.125")
    );
    assert_eq!(
        insights.cost.points[0]
            .estimated_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("0.25")
    );
    assert_eq!(
        insights
            .cost
            .standard_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("1")
    );
    assert_eq!(
        insights.cost.points[0]
            .standard_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("1")
    );
    assert_eq!(diagnostics.items[0].request_share, 0.75);
    assert_eq!(diagnostics.items[1].request_share, 0.25);
}

#[tokio::test]
async fn usage_records_should_tolerate_records_that_fail_billing_enrichment() {
    let now = Utc::now();
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    let invalid_kind = "x".repeat(65);
    store.replace_usage_records(vec![
        total_record(
            "request_invalid_kind",
            Some(&invalid_kind),
            "calculated",
            now,
        ),
        total_record(
            "request_unregistered_kind",
            Some("anthropic"),
            "calculated",
            now,
        ),
        total_record("request_enriched", Some("openai"), "calculated", now),
    ]);
    let services = observability_services_with_calculated_billing(store).await;

    let page = services
        .observability()
        .usage_records(usage_query(now))
        .await
        .expect("one bad record must not fail the whole usage list");

    assert_eq!(page.items.len(), 3);
    assert!(
        matches!(page.items[0].billing, Some(UsageBilling::Total { .. })),
        "invalid Provider kind keeps the stored total"
    );
    assert!(
        matches!(page.items[1].billing, Some(UsageBilling::Total { .. })),
        "unregistered Provider kind keeps the stored total"
    );
    assert!(
        matches!(page.items[2].billing, Some(UsageBilling::Calculated(_))),
        "healthy record is still enriched"
    );
}

#[tokio::test]
async fn usage_records_should_enrich_provider_reported_totals_when_pricing_matches() {
    let now = Utc::now();
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_usage_records(vec![total_record(
        "request_provider_reported",
        Some("openai"),
        "provider_reported",
        now,
    )]);
    let services = observability_services_with_calculated_billing(store).await;

    let page = services
        .observability()
        .usage_records(usage_query(now))
        .await
        .expect("usage records");

    assert!(matches!(
        page.items[0].billing,
        Some(UsageBilling::Calculated(_))
    ));
}

struct FixtureObservabilityStore {
    trend: Mutex<Vec<RequestMetricPoint>>,
    overview: Mutex<UsageOverview>,
    calculated_billing_facts: Mutex<Vec<UsageCalculatedBillingFact>>,
    diagnostics: Mutex<Vec<DiagnosticObservation>>,
    runtime_slots: Mutex<Option<DashboardRuntimeSlots>>,
    usage_records: Mutex<Vec<UsageRecord>>,
}

impl FixtureObservabilityStore {
    fn new(range: TimeRange) -> Self {
        Self {
            trend: Mutex::new(Vec::new()),
            overview: Mutex::new(UsageOverview {
                range,
                requests: RequestMetrics::default(),
                attempts: AttemptMetrics::default(),
                providers: Vec::new(),
            }),
            calculated_billing_facts: Mutex::new(Vec::new()),
            diagnostics: Mutex::new(Vec::new()),
            runtime_slots: Mutex::new(None),
            usage_records: Mutex::new(Vec::new()),
        }
    }

    fn replace_trend(&self, trend: Vec<RequestMetricPoint>) {
        *self.trend.lock().expect("trend") = trend;
    }

    fn replace_overview(&self, overview: UsageOverview) {
        *self.overview.lock().expect("overview") = overview;
    }

    fn replace_calculated_billing_facts(&self, facts: Vec<UsageCalculatedBillingFact>) {
        *self
            .calculated_billing_facts
            .lock()
            .expect("calculated billing facts") = facts;
    }

    fn replace_diagnostics(&self, diagnostics: Vec<DiagnosticObservation>) {
        *self.diagnostics.lock().expect("diagnostics") = diagnostics;
    }

    fn replace_runtime_slots(&self, runtime_slots: Option<DashboardRuntimeSlots>) {
        *self.runtime_slots.lock().expect("runtime slots") = runtime_slots;
    }

    fn replace_usage_records(&self, records: Vec<UsageRecord>) {
        *self.usage_records.lock().expect("usage records") = records;
    }
}

#[async_trait]
impl ObservabilityStore for FixtureObservabilityStore {
    async fn dashboard_summary(&self, range: TimeRange) -> AdminStoreResult<DashboardObservation> {
        Ok(DashboardObservation {
            range,
            requests: RequestMetrics::default(),
            attempts: AttemptMetrics::default(),
            provider_accounts: AccountPoolMetrics::default(),
            trend: self.trend.lock().expect("trend").clone(),
            account_usage: Vec::new(),
            recent_requests: Vec::new(),
        })
    }

    async fn dashboard_runtime_slots(
        &self,
        _: DateTime<Utc>,
    ) -> AdminStoreResult<Option<DashboardRuntimeSlots>> {
        Ok(*self.runtime_slots.lock().expect("runtime slots"))
    }

    async fn dashboard_trend(&self, _: TimeRange) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        Ok(self.trend.lock().expect("trend").clone())
    }

    async fn usage_trend(
        &self,
        _: TimeRange,
        _: UsageFilter,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        Ok(self.trend.lock().expect("trend").clone())
    }

    async fn usage_calculated_billing_facts(
        &self,
        _: TimeRange,
        _: UsageFilter,
    ) -> AdminStoreResult<Vec<UsageCalculatedBillingFact>> {
        Ok(self
            .calculated_billing_facts
            .lock()
            .expect("calculated billing facts")
            .clone())
    }

    async fn list_usage_records(&self, _: UsageQuery) -> AdminStoreResult<UsagePage> {
        let items = self.usage_records.lock().expect("usage records").clone();
        let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
        Ok(UsagePage {
            items,
            total,
            next_cursor: None,
        })
    }

    async fn usage_record_detail(&self, _: &str) -> AdminStoreResult<UsageDetail> {
        Err(super::unavailable("usage detail"))
    }

    async fn usage_summary(&self, _: TimeRange, _: UsageFilter) -> AdminStoreResult<UsageOverview> {
        Ok(self.overview.lock().expect("overview").clone())
    }

    async fn usage_diagnostics(
        &self,
        _: TimeRange,
        _: UsageFilter,
        _: DiagnosticDimension,
    ) -> AdminStoreResult<Vec<DiagnosticObservation>> {
        Ok(self.diagnostics.lock().expect("diagnostics").clone())
    }

    async fn list_ops_errors(&self, _: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage> {
        Err(super::unavailable("ops errors"))
    }
}

struct FixtureSettingsStore;

#[async_trait]
impl SettingsStore for FixtureSettingsStore {
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Ok(RuntimeSettings {
            config_revision: Revision::new(1).expect("revision"),
            model_mappings: Default::default(),
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
            max_concurrent_per_account: 1,
            request_interval_ms: 0,
            rotation_strategy: RotationStrategy::Smart,
            usage_retention_days: 31,
            ops_event_retention_days: 30,
            audit_retention_days: 30,
            updated_at: Utc::now(),
        })
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Err(super::unavailable("admin API key"))
    }

    async fn replace_runtime_settings(
        &self,
        _: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        Err(super::unavailable("settings"))
    }

    async fn replace_admin_api_key(
        &self,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(super::unavailable("admin API key"))
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(super::unavailable("admin API key"))
    }
}

async fn observability_services(store: Arc<FixtureObservabilityStore>) -> AdminServices {
    super::AdminHarness::new()
        .observability(store)
        .settings(Arc::new(FixtureSettingsStore))
        .provider(super::dashboard_profile_provider())
        .build()
        .await
}

async fn observability_services_with_calculated_billing(
    store: Arc<FixtureObservabilityStore>,
) -> AdminServices {
    super::AdminHarness::new()
        .observability(store)
        .settings(Arc::new(FixtureSettingsStore))
        .provider(super::calculated_billing_provider())
        .build()
        .await
}

fn observation_range(end: DateTime<Utc>) -> TimeRange {
    TimeRange::new(end - Duration::hours(24), end).expect("observation range")
}

fn usage_query(now: DateTime<Utc>) -> UsageQuery {
    UsageQuery {
        range: observation_range(now),
        filter: UsageFilter::default(),
        cursor: None,
        page: PageNumber::new(NonZeroU32::new(1).expect("page number")),
        page_size: PageSize::new(50).expect("page size"),
    }
}

fn total_record(
    id: &str,
    provider_kind: Option<&str>,
    source: &str,
    now: DateTime<Utc>,
) -> UsageRecord {
    UsageRecord {
        id: id.to_owned(),
        client_api_key_ref: "key_billing".to_owned(),
        config_revision: 1,
        protocol: "openai".to_owned(),
        operation: "generate".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: "gpt-5.5".to_owned(),
        provider_kind: provider_kind.map(str::to_owned),
        provider_account_ref: None,
        provider_account_name: None,
        provider_account_email: None,
        provider_account_authentication_kind: None,
        upstream_model_id: Some("gpt-5.5".to_owned()),
        upstream_transport: None,
        http_version: None,
        websocket_pool: None,
        service_tier: None,
        provider_metadata_json: None,
        attempt_count: 1,
        upstream_send_state: "sent".to_owned(),
        downstream_committed_at: None,
        outcome: RequestOutcome::Succeeded,
        client_status_code: Some(200),
        upstream_status_code: Some(200),
        client_response_id: None,
        upstream_request_id: None,
        upstream_response_id: None,
        error_kind: None,
        provider_error_code: None,
        error_message: None,
        retry_after_ms: None,
        input_tokens: Some(800),
        output_tokens: Some(200),
        cached_tokens: Some(0),
        cache_write_tokens: Some(0),
        reasoning_tokens: Some(0),
        image_input_tokens: None,
        image_output_tokens: None,
        total_tokens: Some(1_000),
        cost_source: source.to_owned(),
        cost_amount: None,
        cost_currency: None,
        billing: Some(UsageBilling::Total {
            source: source.to_owned(),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: gateway_admin::model::observability::DecimalAmount::from_str("1.25")
                    .expect("billing total"),
            },
        }),
        transport_decision_wait_ms: None,
        connect_ms: None,
        headers_ms: None,
        first_event_ms: None,
        first_reasoning_ms: None,
        first_text_ms: None,
        first_token_ms: None,
        provider_processing_ms: None,
        latency_ms: Some(100),
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        request_kind: None,
        subagent_kind: None,
        compact: false,
        image_generation_requested: false,
        image_generation_succeeded: None,
        started_at: now,
        deadline_at: now,
        completed_at: Some(now),
    }
}

fn health_metric_point(bucket_start: DateTime<Utc>, metrics: RequestMetrics) -> RequestMetricPoint {
    RequestMetricPoint {
        bucket_start,
        granularity: Granularity::FifteenMinutes,
        metrics,
        cost_coverage: CostCoverage::default(),
        costs: Vec::new(),
    }
}

fn diagnostic(name: &str, request_count: u64) -> DiagnosticObservation {
    DiagnosticObservation {
        key: name.to_owned(),
        name: name.to_owned(),
        request_count,
        success_count: request_count,
        failure_count: 0,
        attempt_count: request_count,
        total_tokens: request_count.saturating_mul(100),
        average_latency_ms: Some(100),
        latency_p95_ms: Some(200),
        cost_coverage: CostCoverage::default(),
        costs: Vec::new(),
    }
}

fn quarter_hour_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let elapsed = value.timestamp().rem_euclid(15 * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}
