//! 测试模块入口与共享夹具

use std::{
    str::FromStr as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Timelike as _, Utc};

use gateway_admin::{
    AdminServices,
    model::{
        MutationContext, Revision,
        observability::{
            AccountPoolMetrics, AnalyticsState, AttemptMetrics, CostCoverage, CurrencyCost,
            DashboardAccountUsage, DashboardObservation, DashboardQuery, DashboardRuntimeSlots,
            DiagnosticDimension, DiagnosticObservation, DiagnosticsObservation, Granularity,
            HealthStatus, LatencyPercentiles, ObservabilityPageSize, OpsErrorPage, OpsErrorQuery,
            PercentileMilliseconds, RequestMetricPoint, RequestMetrics, TimeRange, TrendKind,
            UsageBilling, UsageConcurrencyPoint, UsageDetail, UsageFilter,
            UsageInsightsObservation, UsageListRecord, UsageOverview, UsagePage,
            UsageProjectedCostPoint, UsageQuery, UsageSummaryObservation,
        },
        settings::{
            AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RotationStrategy,
            RuntimeSettings,
        },
    },
    ports::store::{AdminStoreResult, ObservabilityStore, SettingsStore},
};

mod errors;

struct FixtureObservabilityStore {
    yesterday: Mutex<RequestMetrics>,
    trend: Mutex<Vec<RequestMetricPoint>>,
    overview: Mutex<UsageOverview>,
    projected_costs: Mutex<Vec<UsageProjectedCostPoint>>,
    concurrency: Mutex<Vec<UsageConcurrencyPoint>>,
    insights_fail: AtomicBool,
    concurrency_selections: Mutex<Vec<bool>>,
    metric_selections: Mutex<Vec<gateway_admin::model::observability::UsageInsightMetric>>,
    summary_fail: AtomicBool,
    diagnostics: Mutex<DiagnosticsObservation>,
    runtime_slots: Mutex<Option<DashboardRuntimeSlots>>,
    summary_observed_at: Mutex<Option<DateTime<Utc>>>,
    summary_query: Mutex<Option<DashboardQuery>>,
    usage_granularities: Mutex<Vec<Granularity>>,
    usage_records: Mutex<Vec<UsageListRecord>>,
    account_usage: Mutex<Vec<DashboardAccountUsage>>,
    dashboard_delay: Mutex<StdDuration>,
    dashboard_summary_calls: AtomicUsize,
    usage_summary_filters: Mutex<Vec<UsageFilter>>,
}

impl FixtureObservabilityStore {
    fn new(range: TimeRange) -> Self {
        Self {
            yesterday: Mutex::new(RequestMetrics::default()),
            trend: Mutex::new(Vec::new()),
            overview: Mutex::new(UsageOverview {
                analytics: Default::default(),
                range,
                requests: RequestMetrics::default(),
                attempts: AttemptMetrics::default(),
                providers: Vec::new(),
            }),
            projected_costs: Mutex::new(Vec::new()),
            concurrency: Mutex::new(Vec::new()),
            insights_fail: AtomicBool::new(false),
            concurrency_selections: Mutex::new(Vec::new()),
            metric_selections: Mutex::new(Vec::new()),
            summary_fail: AtomicBool::new(false),
            diagnostics: Mutex::new(DiagnosticsObservation::default()),
            runtime_slots: Mutex::new(None),
            summary_observed_at: Mutex::new(None),
            summary_query: Mutex::new(None),
            usage_granularities: Mutex::new(Vec::new()),
            usage_records: Mutex::new(Vec::new()),
            account_usage: Mutex::new(Vec::new()),
            dashboard_delay: Mutex::new(StdDuration::ZERO),
            dashboard_summary_calls: AtomicUsize::new(0),
            usage_summary_filters: Mutex::new(Vec::new()),
        }
    }

    fn set_dashboard_delay(&self, delay: StdDuration) {
        *self.dashboard_delay.lock().expect("dashboard delay") = delay;
    }

    fn dashboard_summary_calls(&self) -> usize {
        self.dashboard_summary_calls.load(Ordering::Relaxed)
    }

    fn replace_trend(&self, trend: Vec<RequestMetricPoint>) {
        *self.trend.lock().expect("trend") = trend;
    }

    fn replace_overview(&self, overview: UsageOverview) {
        *self.overview.lock().expect("overview") = overview;
    }

    fn replace_projected_costs(&self, facts: Vec<UsageProjectedCostPoint>) {
        *self.projected_costs.lock().expect("projected costs") = facts;
    }

    fn replace_diagnostics(&self, diagnostics: DiagnosticsObservation) {
        *self.diagnostics.lock().expect("diagnostics") = diagnostics;
    }

    fn replace_runtime_slots(&self, runtime_slots: Option<DashboardRuntimeSlots>) {
        *self.runtime_slots.lock().expect("runtime slots") = runtime_slots;
    }

    fn replace_usage_records(&self, records: Vec<UsageListRecord>) {
        *self.usage_records.lock().expect("usage records") = records;
    }
}

#[async_trait]
impl ObservabilityStore for FixtureObservabilityStore {
    async fn diagnostic_errors(
        &self,
        _target: &gateway_admin::model::diagnostics::DiagnosticTarget,
        _limit: u16,
    ) -> AdminStoreResult<Vec<gateway_admin::model::observability::OpsError>> {
        Err(super::unavailable("diagnostic errors"))
    }

    fn query_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(10)
    }

    async fn dashboard_summary(
        &self,
        query: DashboardQuery,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<DashboardObservation> {
        self.dashboard_summary_calls.fetch_add(1, Ordering::Relaxed);
        *self.summary_query.lock().unwrap() = Some(query.clone());
        *self
            .summary_observed_at
            .lock()
            .expect("summary observed at") = Some(observed_at);
        let dashboard_delay = *self.dashboard_delay.lock().expect("dashboard delay");
        if !dashboard_delay.is_zero() {
            tokio::time::sleep(dashboard_delay).await;
        }
        Ok(DashboardObservation {
            yesterday: self.yesterday.lock().unwrap().clone(),
            analytics: self.overview.lock().expect("overview").analytics.clone(),
            range: query.range,
            totals: Default::default(),
            provider_accounts: AccountPoolMetrics::default(),
            runtime_slots: self.runtime_slots.lock().expect("runtime slots").clone(),
            trend: self.trend.lock().expect("trend").clone(),
            account_usage: self.account_usage.lock().expect("account usage").clone(),
            recent_requests: Vec::new(),
        })
    }

    async fn dashboard_trend(
        &self,
        _: TimeRange,
        _: Granularity,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        Ok(self.trend.lock().expect("trend").clone())
    }

    async fn client_key_statistics(
        &self,
        _: TimeRange,
        _: &gateway_core::policy::ClientApiKeyId,
        _: gateway_admin::model::observability::KeyUsageMetric,
    ) -> AdminStoreResult<gateway_admin::model::observability::KeyStatisticsOverview> {
        Ok(Default::default())
    }

    async fn client_key_models(
        &self,
        _: TimeRange,
        _: &gateway_core::policy::ClientApiKeyId,
    ) -> AdminStoreResult<Vec<gateway_admin::model::observability::KeyModelRow>> {
        Ok(Default::default())
    }

    async fn usage_trend(
        &self,
        _: TimeRange,
        _: UsageFilter,
        granularity: Granularity,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        self.usage_granularities.lock().unwrap().push(granularity);
        Ok(self.trend.lock().expect("trend").clone())
    }

    async fn analytics_cache_generation(&self) -> AdminStoreResult<u64> {
        Ok(self.overview.lock().expect("overview").analytics.generation)
    }

    async fn usage_insights_observation(
        &self,
        _: TimeRange,
        _: UsageFilter,
        granularity: Granularity,
        include_concurrency: bool,
    ) -> AdminStoreResult<UsageInsightsObservation> {
        self.usage_granularities.lock().unwrap().push(granularity);
        self.concurrency_selections
            .lock()
            .unwrap()
            .push(include_concurrency);
        if self.insights_fail.load(Ordering::SeqCst) {
            return Err(super::unavailable("projection read failed"));
        }
        Ok(UsageInsightsObservation {
            overview: self.overview.lock().expect("overview").clone(),
            trend: self.trend.lock().expect("trend").clone(),
            costs: self
                .projected_costs
                .lock()
                .expect("projected costs")
                .clone(),
            concurrency: if include_concurrency {
                self.concurrency.lock().expect("concurrency").clone()
            } else {
                Vec::new()
            },
        })
    }

    async fn usage_metric_observation(
        &self,
        range: gateway_admin::model::observability::TimeRange,
        filter: gateway_admin::model::observability::UsageFilter,
        granularity: gateway_admin::model::observability::Granularity,
        metric: gateway_admin::model::observability::UsageInsightMetric,
    ) -> AdminStoreResult<gateway_admin::model::observability::UsageInsightsObservation> {
        self.metric_selections.lock().unwrap().push(metric);
        self.usage_insights_observation(
            range,
            filter,
            granularity,
            metric == gateway_admin::model::observability::UsageInsightMetric::Load,
        )
        .await
    }

    async fn list_usage_records(&self, query: UsageQuery) -> AdminStoreResult<UsagePage> {
        let items = self.usage_records.lock().expect("usage records").clone();
        let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
        Ok(UsagePage {
            items,
            current_page: query.current_page,
            page_size: query.page_size.get(),
            total,
        })
    }

    async fn usage_record_detail(&self, _: &str) -> AdminStoreResult<UsageDetail> {
        Err(super::unavailable("usage detail"))
    }

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<UsageSummaryObservation> {
        self.usage_summary_filters
            .lock()
            .expect("summary filters")
            .push(filter);
        let delay = *self.dashboard_delay.lock().expect("query delay");
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if self.summary_fail.load(Ordering::SeqCst) {
            return Err(super::unavailable("projection summary failed"));
        }
        let overview = self.overview.lock().expect("overview").clone();
        Ok(UsageSummaryObservation {
            range,
            analytics: overview.analytics,
            requests: (&overview.requests).into(),
            attempts: overview.attempts,
        })
    }

    async fn usage_diagnostics(
        &self,
        _: TimeRange,
        _: UsageFilter,
        _: DiagnosticDimension,
        _: u16,
    ) -> AdminStoreResult<DiagnosticsObservation> {
        Ok(self.diagnostics.lock().expect("diagnostics").clone())
    }

    async fn list_ops_errors(&self, _: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage> {
        Err(super::unavailable("ops errors"))
    }
}

struct FixtureSettingsStore {
    max_concurrent_per_account: u32,
}

#[async_trait]
impl SettingsStore for FixtureSettingsStore {
    async fn load_pricing(&self) -> AdminStoreResult<gateway_admin::model::pricing::StoredPricing> {
        Ok(Default::default())
    }
    async fn sync_pricing(
        &self,
        _: gateway_admin::model::pricing::PricingSyncChanges,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        panic!("unexpected pricing sync")
    }
    async fn update_pricing(
        &self,
        _: gateway_admin::model::pricing::UpdatePricing,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        panic!("unexpected pricing update")
    }
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Ok(RuntimeSettings {
            request_profiles: Default::default(),
            config_revision: Revision::new(1).expect("revision"),
            model_mappings: Default::default(),
            rotation_strategy: RotationStrategy::Smart,
            updated_at: Utc::now(),
            values: gateway_admin::model::settings::RuntimeSettingsValues {
                openai_oauth_guardian_review_model: false,
                openai_request_country: Default::default(),
                codex_privacy_policy: Default::default(),
                request_location_enabled: false,
                request_location: Default::default(),
                refresh_margin_seconds: 300,
                refresh_concurrency: 2,
                max_concurrent_per_account: self.max_concurrent_per_account,
                max_waiting_per_key: 0,
                max_waiting_per_account: 0,
                key_queue_timeout_seconds: 30,
                account_queue_timeout_seconds: 30,
                switch_account_on_queue_failure: false,
                max_retry_after_seconds: 240,
                openai_guardian_reserved_concurrency: 0,
                account_affinity: gateway_core::account::AccountAffinity::Relaxed,
                max_account_rotations: 3,
                session_affinity_ttl_hours: 24,
                responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
                smart_scheduling: gateway_core::account::SmartSchedulingConfig::default(),
                quota_reset_window_weights: gateway_core::account::ResetWindowWeights::default(),
                min_codex_desktop_version: None,
                min_codex_cli_version: None,
                usage_retention_days: 31,
                ops_event_retention_days: 30,
                audit_retention_days: 30,
                account_error_protection: gateway_core::account::AccountErrorProtection::default(),
                account_warmup_enabled: false,
                account_warmup_cron: "0 8 * * *".to_owned(),
                account_warmup_model: None,
                reset_credit_lead_minutes: 10,
                reset_credit_on_low_weekly_quota: false,
                reset_credit_max_weekly_remaining_percent: None,
            },
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
        .settings(Arc::new(FixtureSettingsStore {
            max_concurrent_per_account: 1,
        }))
        .provider(super::dashboard_profile_provider())
        .build()
        .await
}

async fn observability_services_with_calculated_billing(
    store: Arc<FixtureObservabilityStore>,
) -> AdminServices {
    super::AdminHarness::new()
        .observability(store)
        .settings(Arc::new(FixtureSettingsStore {
            max_concurrent_per_account: 1,
        }))
        .provider(super::calculated_billing_provider())
        .build()
        .await
}

fn observation_range(end: DateTime<Utc>) -> TimeRange {
    TimeRange::new(end - Duration::hours(24), end).expect("observation range")
}

fn usage_query(now: DateTime<Utc>) -> UsageQuery {
    UsageQuery {
        status: Default::default(),
        range: observation_range(now),
        filter: UsageFilter::default(),
        current_page: 1,
        page_size: ObservabilityPageSize::new(50).expect("page size"),
    }
}

fn total_record(
    id: &str,
    provider_kind: Option<&str>,
    source: &str,
    now: DateTime<Utc>,
) -> UsageListRecord {
    UsageListRecord {
        provider_account_upstream_peer: None,
        has_upstream_error: false,
        error_code: None,
        error_message: None,
        error_kind: None,
        websocket_close_code: None,
        continuation_unavailable_reason: None,
        upstream_connection_exit_reason: None,
        recovered: false,
        client_status_code: None,
        upstream_status_code: None,
        outcome: gateway_admin::model::observability::RequestOutcome::Succeeded,
        client_api_key_name: Some("Production".to_owned()),
        id: id.to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: Some("gpt-5.5".to_owned()),
        provider_kind: provider_kind.map(str::to_owned),
        provider_account_ref: None,
        provider_account_name: None,
        provider_account_email: None,
        provider_account_notes: None,
        provider_account_plan_type: None,
        provider_account_plan_type_display: None,
        provider_account_authentication_kind: None,
        upstream_model_id: Some("gpt-5.5".to_owned()),
        upstream_transport: None,
        service_tier: None,
        input_tokens: Some(800),
        output_tokens: Some(200),
        cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0),
        reasoning_output_tokens: Some(0),
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
        upstream_response_ms: None,
        upstream_api_overhead_ms: None,
        upstream_engine_ms: None,
        latency_ms: Some(100),
        admission_decision_ms: None,
        account_selection_wait_ms: None,
        capacity_used_slots: None,
        capacity_total_slots: None,
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        subagent_kind: None,
        compact: false,
        started_at: now,
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
        first_token_p95_ms: None,
        cancelled_count: 0,
        incomplete_count: 0,
        caller_error_count: 0,
        retry_count: 0,
        retried_request_count: 0,
        account_provider_kind: None,
        account_authentication_kind: None,
        account_plan_type: None,
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

mod service;

mod dashboard;

mod store_cache;

mod insights;
