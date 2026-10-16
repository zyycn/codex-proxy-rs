//! Pg 观测查询与 Admin 观测端口适配

use gateway_core::provider_ports::ProviderCooldownPort;
use std::sync::Arc;

use super::*;

use crate::postgres::{ProviderAccountStatus, load_account_statuses, load_cooldown};

use crate::redis::{CredentialLeaseRepository as _, RedisCredentialLeaseRepository};

#[derive(Clone)]
pub struct PgObservabilityRepository {
    timezone: gateway_core::time::DeploymentTimeZone,
    pool: PgPool,
    cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
    runtime_signals: Option<RedisCredentialLeaseRepository>,
    query_budget: ObservabilityQueryBudget,
}

impl PgObservabilityRepository {
    #[must_use]
    pub fn new(
        pool: PgPool,
        cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
        query_budget: ObservabilityQueryBudget,
        runtime_signals: Option<RedisCredentialLeaseRepository>,
    ) -> Self {
        Self {
            timezone: Default::default(),
            pool,
            cooldowns,
            runtime_signals,
            query_budget,
        }
    }

    #[must_use]
    pub fn with_timezone(mut self, timezone: gateway_core::time::DeploymentTimeZone) -> Self {
        self.timezone = timezone;
        self
    }

    /// 从账号事实和当前冷却一次性派生 Dashboard 五态；不在 SQL 中复制状态机
    async fn account_status_snapshot(
        &self,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<(ProviderAccountMetrics, Vec<ProviderAccountStatus>)> {
        let accounts = self
            .query_budget
            .run(
                &self.pool,
                "load dashboard provider accounts",
                |mut connection| async move { load_account_statuses(&mut *connection).await },
            )
            .await?;
        let now = observed_at.into();
        let cooldown = load_cooldown(self.cooldowns.as_deref(), &accounts, now).await;
        let mut metrics = ProviderAccountMetrics {
            total: u64::try_from(accounts.len()).unwrap_or(u64::MAX),
            ..ProviderAccountMetrics::default()
        };
        let mut normal_accounts = Vec::new();
        for mut account in accounts {
            account.facts.cooldown = cooldown.get(&account.id).copied();
            let projection = gateway_core::account::resolve_account_status(&account.facts, now);
            match projection.status {
                gateway_core::account::AccountStatus::Normal => {
                    metrics.normal = metrics.normal.saturating_add(1);
                    normal_accounts.push(account);
                }
                gateway_core::account::AccountStatus::QuotaExhausted => {
                    metrics.quota_exhausted = metrics.quota_exhausted.saturating_add(1);
                }
                gateway_core::account::AccountStatus::RateLimited => {
                    metrics.rate_limited = metrics.rate_limited.saturating_add(1);
                }
                gateway_core::account::AccountStatus::Disabled => {
                    metrics.disabled = metrics.disabled.saturating_add(1);
                }
                gateway_core::account::AccountStatus::Error => {
                    metrics.error = metrics.error.saturating_add(1);
                }
            }
        }
        Ok((metrics, normal_accounts))
    }
    async fn dashboard_runtime_slots(
        &self,
        normal_accounts: &[ProviderAccountStatus],
    ) -> StoreResult<admin_observability::DashboardRuntimeSlots> {
        let inherited_accounts = u64::try_from(
            normal_accounts
                .iter()
                .filter(|account| account.concurrency_limit.is_none())
                .count(),
        )
        .map_err(|_| invalid("inherited account count overflows u64"))?;
        let overridden_slots = normal_accounts.iter().fold(0_u64, |total, account| {
            total.saturating_add(
                account
                    .concurrency_limit
                    .map_or(0, |limit| u64::from(limit.get())),
            )
        });
        let used_slots = if normal_accounts.is_empty() {
            Some(0)
        } else if let Some(runtime_signals) = &self.runtime_signals {
            let ids = normal_accounts
                .iter()
                .map(|account| account.id.clone())
                .collect::<Vec<_>>();
            runtime_signals
                .credential_runtime_signals(&ids)
                .await
                .ok()
                .map(|signals| {
                    signals.into_iter().fold(0_u64, |total, signal| {
                        total.saturating_add(u64::from(signal.in_flight))
                    })
                })
        } else {
            None
        };
        Ok(admin_observability::DashboardRuntimeSlots {
            inherited_accounts,
            overridden_slots,
            used_slots,
        })
    }
}

/// `gateway-admin` 观测端口的 PostgreSQL adapter
///
/// SQL 查询与内部投影继续由 [`PgObservabilityRepository`] 唯一拥有；本类型只负责
/// Admin UTC 领域模型与持久化投影之间的无格式化转换
#[derive(Clone)]
pub struct PgAdminObservabilityStore {
    repository: PgObservabilityRepository,
}

impl PgAdminObservabilityStore {
    #[must_use]
    pub fn new(
        pool: PgPool,
        runtime_signals: Option<RedisCredentialLeaseRepository>,
        cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
        query_budget: ObservabilityQueryBudget,
    ) -> Self {
        Self {
            repository: PgObservabilityRepository::new(
                pool,
                cooldowns,
                query_budget,
                runtime_signals,
            ),
        }
    }
    #[must_use]
    pub fn with_timezone(mut self, timezone: gateway_core::time::DeploymentTimeZone) -> Self {
        self.repository = self.repository.with_timezone(timezone);
        self
    }
}

impl PgObservabilityRepository {
    pub async fn dashboard_summary(
        &self,
        query: admin_observability::DashboardQuery,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<DashboardObservation> {
        let range = store_range(query.range)?;
        let account_usage_query = ProviderAccountUsageQuery::recent(range, query.account_limit)?
            .with_hourly_request_buckets()?;
        let recent_query = UsageRecordQuery {
            status: Default::default(),
            range,
            filter: store_usage_filter(query.recent_request_filter),
            current_page: 1,
            page_size: ObservabilityPageSize::new(query.recent_request_limit)
                .map_err(|_| invalid("invalid dashboard page size"))?,
        };
        let (totals, trend, analytics, account_usage) = self
            .query_budget
            .run(
                &self.pool,
                "load dashboard analytics",
                |mut connection| async move {
                    let mut snapshot = analytics::AnalyticsSnapshot::new(&mut connection).await?;
                    let totals = snapshot.totals().await?;
                    let trend = snapshot
                        .trend(
                            range,
                            &UsageRecordFilter::default(),
                            query.granularity,
                            self.timezone,
                            false,
                        )
                        .await?;
                    let account_usage =
                        provider_account_usage(&mut snapshot, account_usage_query).await?;
                    let metadata = snapshot.metadata();
                    snapshot.finish().await?;
                    Ok((totals, trend, metadata, account_usage))
                },
            )
            .await?;
        let (provider_accounts, normal_accounts) =
            self.account_status_snapshot(observed_at).await?;
        let recent_requests = self
            .query_budget
            .run(
                &self.pool,
                "load dashboard recent requests",
                |mut connection| async move {
                    list_usage_record_items(&mut *connection, &recent_query, None).await
                },
            )
            .await?;
        let runtime_slots = self.dashboard_runtime_slots(&normal_accounts).await?;
        Ok(DashboardObservation {
            analytics,
            range,
            totals,
            provider_accounts,
            runtime_slots,
            trend,
            account_usage,
            recent_requests,
        })
    }

    pub async fn dashboard_trend(
        &self,
        range: ObservabilityRange,
        granularity: admin_observability::Granularity,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        self.query_budget
            .run(
                &self.pool,
                "load dashboard analytics trend",
                |mut connection| async move {
                    let mut snapshot = analytics::AnalyticsSnapshot::new(&mut connection).await?;
                    let points = snapshot
                        .trend(
                            range,
                            &UsageRecordFilter::default(),
                            granularity,
                            self.timezone,
                            false,
                        )
                        .await?;
                    snapshot.finish().await?;
                    Ok(points)
                },
            )
            .await
    }

    pub async fn usage_trend(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
        granularity: admin_observability::Granularity,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        self.query_budget
            .run(
                &self.pool,
                "load analytics trend",
                |mut connection| async move {
                    let mut snapshot = analytics::AnalyticsSnapshot::new(&mut connection).await?;
                    let points = snapshot
                        .trend(range, &filter, granularity, self.timezone, true)
                        .await?;
                    snapshot.finish().await?;
                    Ok(points)
                },
            )
            .await
    }

    pub async fn usage_insights_observation(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
        granularity: admin_observability::Granularity,
    ) -> StoreResult<admin_observability::UsageInsightsObservation> {
        self.query_budget
            .run(
                &self.pool,
                "load usage insights snapshot",
                |mut connection| async move {
                    let mut snapshot = analytics::AnalyticsSnapshot::new(&mut connection).await?;
                    let (mut overview, trend, costs) = snapshot
                        .insights(range, &filter, granularity, self.timezone)
                        .await?;
                    overview.analytics = snapshot.metadata();
                    snapshot.finish().await?;
                    Ok(admin_observability::UsageInsightsObservation {
                        overview: admin_usage_overview(overview),
                        trend: trend.into_iter().map(admin_request_metric_point).collect(),
                        costs,
                    })
                },
            )
            .await
    }

    pub async fn provider_account_usage(
        &self,
        query: ProviderAccountUsageQuery,
    ) -> StoreResult<Vec<ProviderAccountUsageObservation>> {
        self.query_budget
            .run(
                &self.pool,
                "load provider account usage",
                |mut connection| async move {
                    let mut snapshot =
                        analytics::AnalyticsSnapshot::new_for_accounts(&mut connection).await?;
                    let usage = provider_account_usage(&mut snapshot, query).await?;
                    snapshot.finish().await?;
                    Ok(usage)
                },
            )
            .await
    }

    pub async fn list_usage_records(
        &self,
        query: UsageRecordQuery,
    ) -> StoreResult<UsageRecordPage> {
        self.query_budget
            .run(
                &self.pool,
                "list usage records",
                |mut connection| async move { list_usage_records(&mut connection, query).await },
            )
            .await
    }

    pub async fn usage_record_detail(&self, request_id: &str) -> StoreResult<UsageRecordDetail> {
        self.query_budget
            .run(
                &self.pool,
                "load usage record detail",
                |mut connection| async move { usage_record_detail(&mut connection, request_id).await },
            )
            .await
    }

    pub async fn usage_summary(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<UsageOverview> {
        self.query_budget
            .run(
                &self.pool,
                "load usage analytics",
                |mut connection| async move {
                    let mut snapshot = analytics::AnalyticsSnapshot::new(&mut connection).await?;
                    let overview = snapshot.overview(range, &filter).await?;
                    snapshot.finish().await?;
                    Ok(overview)
                },
            )
            .await
    }

    pub async fn usage_diagnostics(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
        dimension: DiagnosticDimension,
        limit: u16,
    ) -> StoreResult<DiagnosticsObservation> {
        self.query_budget
            .run(
                &self.pool,
                "load analytics diagnostics",
                |mut connection| async move {
                    let mut snapshot = analytics::AnalyticsSnapshot::new(&mut connection).await?;
                    let observation = snapshot
                        .diagnostics(range, &filter, dimension, limit)
                        .await?;
                    snapshot.finish().await?;
                    Ok(observation)
                },
            )
            .await
    }

    pub async fn list_ops_errors(&self, query: OpsErrorQuery) -> StoreResult<OpsErrorPage> {
        self.query_budget
            .run(&self.pool, "list ops errors", |mut connection| async move {
                list_ops_errors(&mut connection, query).await
            })
            .await
    }
}

#[async_trait]
impl AdminObservabilityStore for PgAdminObservabilityStore {
    async fn diagnostic_errors(
        &self,
        target: &gateway_admin::model::diagnostics::DiagnosticTarget,
        limit: u16,
    ) -> AdminStoreResult<Vec<admin_observability::OpsError>> {
        let target = target.clone();
        self.repository.query_budget.run(&self.repository.pool, "export diagnostic errors", |mut connection| async move {
            diagnostic_errors(&mut connection, &target, limit).await
        }).await.map_err(observability_error)
    }

    fn query_timeout(&self) -> std::time::Duration {
        self.repository.query_budget.query_timeout()
    }

    async fn analytics_cache_generation(&self) -> AdminStoreResult<u64> {
        self.repository
            .query_budget
            .run(
                &self.repository.pool,
                "read analytics cache generation",
                |mut connection| async move {
                    let generation: i64 = sqlx::query_scalar(
                        "select generation from request_analytics_build_state where singleton",
                    )
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|error| {
                        postgres_unavailable("read analytics cache generation", error)
                    })?;
                    u64::try_from(generation).map_err(|_| invalid("invalid analytics generation"))
                },
            )
            .await
            .map_err(observability_error)
    }

    async fn dashboard_summary(
        &self,
        query: admin_observability::DashboardQuery,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<admin_observability::DashboardObservation> {
        let observation = self
            .repository
            .dashboard_summary(query, observed_at)
            .await
            .map_err(observability_error)?;
        Ok(admin_dashboard_observation(observation))
    }

    async fn dashboard_trend(
        &self,
        range: admin_observability::TimeRange,
        granularity: admin_observability::Granularity,
    ) -> AdminStoreResult<Vec<admin_observability::RequestMetricPoint>> {
        Ok(self
            .repository
            .dashboard_trend(
                store_range(range).map_err(observability_error)?,
                granularity,
            )
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_request_metric_point)
            .collect())
    }

    async fn usage_trend(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
        granularity: admin_observability::Granularity,
    ) -> AdminStoreResult<Vec<admin_observability::RequestMetricPoint>> {
        Ok(self
            .repository
            .usage_trend(
                store_range(range).map_err(observability_error)?,
                store_usage_filter(filter),
                granularity,
            )
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_request_metric_point)
            .collect())
    }

    async fn usage_insights_observation(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
        granularity: admin_observability::Granularity,
    ) -> AdminStoreResult<admin_observability::UsageInsightsObservation> {
        self.repository
            .usage_insights_observation(
                store_range(range).map_err(observability_error)?,
                store_usage_filter(filter),
                granularity,
            )
            .await
            .map_err(observability_error)
    }

    async fn list_usage_records(
        &self,
        query: admin_observability::UsageQuery,
    ) -> AdminStoreResult<admin_observability::UsagePage> {
        let page = self
            .repository
            .list_usage_records(store_usage_query(query)?)
            .await
            .map_err(observability_error)?;
        Ok(page)
    }

    async fn usage_record_detail(
        &self,
        request_id: &str,
    ) -> AdminStoreResult<admin_observability::UsageDetail> {
        let detail = self
            .repository
            .usage_record_detail(request_id)
            .await
            .map_err(observability_error)?;
        Ok(detail)
    }

    async fn usage_summary(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
    ) -> AdminStoreResult<admin_observability::UsageOverview> {
        let overview = self
            .repository
            .usage_summary(
                store_range(range).map_err(observability_error)?,
                store_usage_filter(filter),
            )
            .await
            .map_err(observability_error)?;
        Ok(admin_usage_overview(overview))
    }

    async fn usage_diagnostics(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
        dimension: admin_observability::DiagnosticDimension,
        limit: u16,
    ) -> AdminStoreResult<admin_observability::DiagnosticsObservation> {
        let observation = self
            .repository
            .usage_diagnostics(
                store_range(range).map_err(observability_error)?,
                store_usage_filter(filter),
                dimension,
                limit,
            )
            .await
            .map_err(observability_error)?;
        Ok(admin_diagnostics_observation(observation))
    }

    async fn list_ops_errors(
        &self,
        query: admin_observability::OpsErrorQuery,
    ) -> AdminStoreResult<admin_observability::OpsErrorPage> {
        let page = self
            .repository
            .list_ops_errors(store_ops_error_query(query)?)
            .await
            .map_err(observability_error)?;
        Ok(page)
    }
}

pub(crate) fn observability_error(
    error: StoreError,
) -> gateway_admin::ports::store::AdminStoreError {
    admin_store_error("observability", error)
}
