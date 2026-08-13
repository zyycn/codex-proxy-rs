//! Pg 观测 adapter：实现 `ObservabilityRepository` 与 `AdminObservabilityStore`。

use gateway_core::provider_ports::ProviderCooldownPort;
use std::sync::Arc;

use super::*;

use crate::postgres::{
    PgProviderAccountRepository, ProviderAccountRepository, ProviderAccountSummary,
    account_status_projection, load_rate_limited_until,
};

use crate::redis::{CredentialLeaseRepository as _, RedisCredentialLeaseRepository};

#[derive(Clone)]
pub struct PgObservabilityRepository {
    pool: PgPool,
    cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
}

impl PgObservabilityRepository {
    #[must_use]
    pub const fn new(pool: PgPool, cooldowns: Option<Arc<dyn ProviderCooldownPort>>) -> Self {
        Self { pool, cooldowns }
    }

    /// 从账号事实和当前冷却一次性派生 Dashboard 五态；不在 SQL 中复制状态机。
    async fn account_status_snapshot(
        &self,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<(ProviderAccountMetrics, Vec<ProviderAccountSummary>)> {
        let accounts = PgProviderAccountRepository::new(self.pool.clone())
            .list_provider_accounts(None, true)
            .await?;
        let now = observed_at.into();
        let rate_limited_until =
            load_rate_limited_until(self.cooldowns.as_deref(), &accounts, now).await;
        let mut metrics = ProviderAccountMetrics {
            total: u64::try_from(accounts.len()).unwrap_or(u64::MAX),
            ..ProviderAccountMetrics::default()
        };
        let mut normal_accounts = Vec::new();
        for account in &accounts {
            let projection = account_status_projection(
                account,
                now,
                rate_limited_until.get(&account.id).copied(),
            );
            match projection.status {
                gateway_core::engine::credential::AccountStatus::Normal => {
                    metrics.normal = metrics.normal.saturating_add(1);
                    normal_accounts.push(account.clone());
                }
                gateway_core::engine::credential::AccountStatus::QuotaExhausted => {
                    metrics.quota_exhausted = metrics.quota_exhausted.saturating_add(1);
                }
                gateway_core::engine::credential::AccountStatus::RateLimited => {
                    metrics.rate_limited = metrics.rate_limited.saturating_add(1);
                }
                gateway_core::engine::credential::AccountStatus::Disabled => {
                    metrics.disabled = metrics.disabled.saturating_add(1);
                }
                gateway_core::engine::credential::AccountStatus::Error => {
                    metrics.error = metrics.error.saturating_add(1);
                }
            }
        }
        Ok((metrics, normal_accounts))
    }
}

/// `gateway-admin` 观测端口的 PostgreSQL adapter。
///
/// SQL 查询与内部投影继续由 [`PgObservabilityRepository`] 唯一拥有；本类型只负责
/// Admin UTC 领域模型与持久化投影之间的无格式化转换。
#[derive(Clone)]
pub struct PgAdminObservabilityStore {
    repository: PgObservabilityRepository,
    runtime_signals: Option<RedisCredentialLeaseRepository>,
}

impl PgAdminObservabilityStore {
    #[must_use]
    pub fn new(
        pool: PgPool,
        runtime_signals: Option<RedisCredentialLeaseRepository>,
        cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
    ) -> Self {
        Self {
            repository: PgObservabilityRepository::new(pool, cooldowns),
            runtime_signals,
        }
    }
}

#[async_trait]
impl ObservabilityRepository for PgObservabilityRepository {
    async fn dashboard_summary(
        &self,
        range: ObservabilityRange,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<DashboardObservation> {
        let filter = UsageRecordFilter::default();
        let account_usage_range = ObservabilityRange::new(
            range.end - TimeDelta::hours(ACCOUNT_USAGE_TIMELINE_HOURS),
            range.end,
        )?;
        let account_usage_query =
            ProviderAccountUsageQuery::recent(account_usage_range, DASHBOARD_ACCOUNT_LIMIT)?
                .with_hourly_request_buckets()?;
        let recent_query = UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                outcome: Some("succeeded".to_owned()),
                ..UsageRecordFilter::default()
            },
            cursor: None,
            page: ObservabilityPageNumber::new(1)?,
            page_size: ObservabilityPageSize::new(10)?,
        };
        // 每批最多三路并发，既缩短概览渲染路径，也不与数据面写路径争抢整个连接池。
        let (requests, attempts, totals) = futures::try_join!(
            request_metrics(&self.pool, range, &filter),
            attempt_metrics(&self.pool, range, &filter),
            dashboard_totals(&self.pool),
        )?;
        let (provider_accounts, _) = self.account_status_snapshot(observed_at).await?;
        let (trend, account_usage, recent_requests) = futures::try_join!(
            request_metric_series(&self.pool, range, &filter),
            provider_account_usage(&self.pool, account_usage_query),
            async { Ok(list_usage_records(&self.pool, recent_query).await?.items) },
        )?;
        Ok(DashboardObservation {
            range,
            requests,
            attempts,
            totals,
            provider_accounts,
            trend,
            account_usage,
            recent_requests,
        })
    }

    async fn dashboard_trend(
        &self,
        range: ObservabilityRange,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        request_metric_series(&self.pool, range, &UsageRecordFilter::default()).await
    }

    async fn usage_trend(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        request_metric_series(&self.pool, range, &filter).await
    }

    async fn usage_calculated_billing_facts(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<Vec<CalculatedUsageBillingFact>> {
        calculated_usage_billing_facts(&self.pool, range, &filter).await
    }

    async fn provider_account_usage(
        &self,
        query: ProviderAccountUsageQuery,
    ) -> StoreResult<Vec<ProviderAccountUsageObservation>> {
        provider_account_usage(&self.pool, query).await
    }

    async fn list_usage_records(&self, query: UsageRecordQuery) -> StoreResult<UsageRecordPage> {
        list_usage_records(&self.pool, query).await
    }

    async fn usage_record_detail(&self, request_id: &str) -> StoreResult<UsageRecordDetail> {
        usage_record_detail(&self.pool, request_id).await
    }

    async fn usage_summary(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<UsageOverview> {
        filter.validate()?;
        let requests = request_metrics(&self.pool, range, &filter).await?;
        let attempts = attempt_metrics(&self.pool, range, &filter).await?;
        let providers = provider_observations(&self.pool, range, &filter).await?;
        Ok(UsageOverview {
            range,
            requests,
            attempts,
            providers,
        })
    }

    async fn usage_diagnostics(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
        dimension: DiagnosticDimension,
    ) -> StoreResult<Vec<DiagnosticObservation>> {
        usage_diagnostics(&self.pool, range, &filter, dimension).await
    }

    async fn list_ops_errors(&self, query: OpsErrorQuery) -> StoreResult<OpsErrorPage> {
        list_ops_errors(&self.pool, query).await
    }
}

#[async_trait]
impl AdminObservabilityStore for PgAdminObservabilityStore {
    async fn dashboard_summary(
        &self,
        range: admin_observability::TimeRange,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<admin_observability::DashboardObservation> {
        let observation = self
            .repository
            .dashboard_summary(store_range(range)?, observed_at)
            .await
            .map_err(observability_error)?;
        admin_dashboard_observation(observation)
    }

    async fn dashboard_runtime_slots(
        &self,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<Option<admin_observability::DashboardRuntimeSlots>> {
        let (_, normal_accounts) = self
            .repository
            .account_status_snapshot(observed_at)
            .await
            .map_err(observability_error)?;
        let inherited_accounts = u64::try_from(
            normal_accounts
                .iter()
                .filter(|account| account.concurrency_limit.is_none())
                .count(),
        )
        .map_err(|_| observability_error(invalid("inherited account count overflows u64")))?;
        let overridden_slots = normal_accounts.iter().fold(0_u64, |total, account| {
            total.saturating_add(
                account
                    .concurrency_limit
                    .map_or(0, |limit| u64::from(limit.get())),
            )
        });
        if normal_accounts.is_empty() {
            return Ok(Some(admin_observability::DashboardRuntimeSlots {
                inherited_accounts,
                overridden_slots,
                used_slots: Some(0),
            }));
        }
        let normal_account_ids = normal_accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let Some(runtime_signals) = &self.runtime_signals else {
            return Ok(Some(admin_observability::DashboardRuntimeSlots {
                inherited_accounts,
                overridden_slots,
                used_slots: None,
            }));
        };
        let signals = match runtime_signals
            .credential_runtime_signals(&normal_account_ids)
            .await
        {
            Ok(signals) => signals,
            Err(_) => {
                return Ok(Some(admin_observability::DashboardRuntimeSlots {
                    inherited_accounts,
                    overridden_slots,
                    used_slots: None,
                }));
            }
        };
        let used_slots = signals.into_iter().fold(0_u64, |total, signal| {
            total.saturating_add(u64::from(signal.in_flight))
        });
        Ok(Some(admin_observability::DashboardRuntimeSlots {
            inherited_accounts,
            overridden_slots,
            used_slots: Some(used_slots),
        }))
    }

    async fn dashboard_trend(
        &self,
        range: admin_observability::TimeRange,
    ) -> AdminStoreResult<Vec<admin_observability::RequestMetricPoint>> {
        self.repository
            .dashboard_trend(store_range(range)?)
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_request_metric_point)
            .collect()
    }

    async fn usage_trend(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
    ) -> AdminStoreResult<Vec<admin_observability::RequestMetricPoint>> {
        self.repository
            .usage_trend(store_range(range)?, store_usage_filter(filter))
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_request_metric_point)
            .collect()
    }

    async fn usage_calculated_billing_facts(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
    ) -> AdminStoreResult<Vec<admin_observability::UsageCalculatedBillingFact>> {
        self.repository
            .usage_calculated_billing_facts(store_range(range)?, store_usage_filter(filter))
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_calculated_usage_billing_fact)
            .collect()
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
        admin_usage_page(page)
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
        admin_usage_detail(detail)
    }

    async fn usage_summary(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
    ) -> AdminStoreResult<admin_observability::UsageOverview> {
        let overview = self
            .repository
            .usage_summary(store_range(range)?, store_usage_filter(filter))
            .await
            .map_err(observability_error)?;
        admin_usage_overview(overview)
    }

    async fn usage_diagnostics(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
        dimension: admin_observability::DiagnosticDimension,
    ) -> AdminStoreResult<Vec<admin_observability::DiagnosticObservation>> {
        self.repository
            .usage_diagnostics(
                store_range(range)?,
                store_usage_filter(filter),
                store_diagnostic_dimension(dimension),
            )
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_diagnostic_observation)
            .collect()
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
        admin_ops_error_page(page)
    }
}

pub(crate) fn observability_error(
    error: StoreError,
) -> gateway_admin::ports::store::AdminStoreError {
    admin_store_error("observability", error)
}
