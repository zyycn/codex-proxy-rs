use std::{
    str::FromStr as _,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Timelike as _, Utc};

use gateway_admin::{
    AdminServices,
    model::{
        MutationContext, Revision,
        observability::{
            AccountPoolMetrics, AttemptMetrics, CostCoverage, CurrencyCost, DashboardObservation,
            DashboardRuntimeSlots, DiagnosticDimension, DiagnosticObservation, Granularity,
            HealthStatus, OpsErrorPage, OpsErrorQuery, RequestMetricPoint, RequestMetrics,
            TimeRange, TrendKind, UsageDetail, UsageFilter, UsageOverview, UsagePage, UsageQuery,
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
    store.replace_diagnostics(vec![diagnostic("openai", 3), diagnostic("xai", 1)]);
    let services = observability_services(store).await;

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
    assert_eq!(diagnostics.items[0].request_share, 0.75);
    assert_eq!(diagnostics.items[1].request_share, 0.25);
}

struct FixtureObservabilityStore {
    trend: Mutex<Vec<RequestMetricPoint>>,
    overview: Mutex<UsageOverview>,
    diagnostics: Mutex<Vec<DiagnosticObservation>>,
    runtime_slots: Mutex<Option<DashboardRuntimeSlots>>,
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
            diagnostics: Mutex::new(Vec::new()),
            runtime_slots: Mutex::new(None),
        }
    }

    fn replace_trend(&self, trend: Vec<RequestMetricPoint>) {
        *self.trend.lock().expect("trend") = trend;
    }

    fn replace_overview(&self, overview: UsageOverview) {
        *self.overview.lock().expect("overview") = overview;
    }

    fn replace_diagnostics(&self, diagnostics: Vec<DiagnosticObservation>) {
        *self.diagnostics.lock().expect("diagnostics") = diagnostics;
    }

    fn replace_runtime_slots(&self, runtime_slots: Option<DashboardRuntimeSlots>) {
        *self.runtime_slots.lock().expect("runtime slots") = runtime_slots;
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

    async fn list_usage_records(&self, _: UsageQuery) -> AdminStoreResult<UsagePage> {
        Err(super::unavailable("usage records"))
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
        _: Revision,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(super::unavailable("admin API key"))
    }

    async fn delete_admin_api_key(
        &self,
        _: Revision,
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

fn observation_range(end: DateTime<Utc>) -> TimeRange {
    TimeRange::new(end - Duration::hours(24), end).expect("observation range")
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
        name: name.to_owned(),
        request_count,
        success_count: request_count,
        failure_count: 0,
        attempt_count: request_count,
        total_tokens: request_count.saturating_mul(100),
        average_latency_ms: Some(100),
        cost_coverage: CostCoverage::default(),
        costs: Vec::new(),
    }
}

fn china_day_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let elapsed = (value.timestamp() + 8 * 60 * 60).rem_euclid(24 * 60 * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}

fn quarter_hour_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let elapsed = value.timestamp().rem_euclid(15 * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}
