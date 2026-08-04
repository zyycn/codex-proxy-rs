//! Pg 观测 adapter：实现 `ObservabilityRepository` 与 `AdminObservabilityStore`。

use std::sync::Arc;
use std::time::SystemTime;

use gateway_core::provider_ports::ProviderCooldownPort;

use super::*;

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

    /// 账号池指标：SQL 聚合后，仅用 Redis 429 冷却重分类仍可调度的 ready 账号。
    /// 冷却中从 `active` 或 `quota_exhausted` 移到 `rate_limited`；冷却不可用时保留 SQL 分类。
    async fn provider_account_metrics_with_cooldowns(
        &self,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<ProviderAccountMetrics> {
        let base = provider_account_metrics(&self.pool, observed_at).await?;
        let Some(cooldowns) = &self.cooldowns else {
            return Ok(base);
        };
        let candidates = schedulable_metric_candidates(&self.pool, observed_at).await?;
        if candidates.is_empty() {
            return Ok(base);
        }
        // Redis 只能把 SQL 已归入 active/quota_exhausted 的候选移到冷却桶；
        // 其余状态不重建，避免 Redis 失败或脏 ID 时丢失持久化计数。
        let mut active = base.active;
        let mut rate_limited = base.rate_limited;
        let mut quota_exhausted = base.quota_exhausted;
        let now = SystemTime::now();
        for (account_id, revision, quota_reached) in candidates {
            let Ok(account_id) =
                gateway_core::engine::credential::ProviderAccountId::new(account_id)
            else {
                continue;
            };
            let cooling = cooldowns
                .read(&account_id)
                .await
                .ok()
                .flatten()
                .filter(|cooldown| cooldown.credential_revision().get() as i64 == revision)
                .filter(|cooldown| cooldown.until() > now)
                .is_some();
            if !cooling {
                continue;
            }
            if quota_reached {
                if quota_exhausted == 0 {
                    continue;
                }
                quota_exhausted -= 1;
            } else {
                if active == 0 {
                    continue;
                }
                active -= 1;
            }
            rate_limited += 1;
        }
        Ok(ProviderAccountMetrics {
            total: base.total,
            enabled: base.enabled,
            unavailable: rate_limited
                + quota_exhausted
                + base.expired
                + base.invalid
                + base.disabled
                + base.banned,
            active,
            rate_limited,
            expired: base.expired,
            invalid: base.invalid,
            quota_exhausted,
            disabled: base.disabled,
            banned: base.banned,
        })
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
        // 分两批并发：页面延迟从六路之和降为两批各自最慢者之和，同时单次
        // 渲染最多占用 3 个池连接，不与数据面写路径争抢整个连接池。
        let (requests, attempts) = futures::try_join!(
            request_metrics(&self.pool, range, &filter),
            attempt_metrics(&self.pool, range, &filter),
        )?;
        let provider_accounts = self
            .provider_account_metrics_with_cooldowns(range.end)
            .await?;
        let (trend, account_usage, recent_requests) = futures::try_join!(
            request_metric_series(&self.pool, range, &filter),
            provider_account_usage(&self.pool, account_usage_query),
            async { Ok(list_usage_records(&self.pool, recent_query).await?.items) },
        )?;
        Ok(DashboardObservation {
            range,
            requests,
            attempts,
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
    ) -> AdminStoreResult<admin_observability::DashboardObservation> {
        let observation = self
            .repository
            .dashboard_summary(store_range(range)?)
            .await
            .map_err(observability_error)?;
        admin_dashboard_observation(observation)
    }

    async fn dashboard_runtime_slots(
        &self,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<Option<admin_observability::DashboardRuntimeSlots>> {
        let active_account_ids = active_provider_account_ids(&self.repository.pool, observed_at)
            .await
            .map_err(observability_error)?;
        let active_accounts = u64::try_from(active_account_ids.len())
            .map_err(|_| observability_error(invalid("active account count overflows u64")))?;
        if active_account_ids.is_empty() {
            return Ok(Some(admin_observability::DashboardRuntimeSlots {
                active_accounts,
                used_slots: Some(0),
            }));
        }
        let Some(runtime_signals) = &self.runtime_signals else {
            return Ok(None);
        };
        let signals = match runtime_signals
            .credential_runtime_signals(&active_account_ids)
            .await
        {
            Ok(signals) => signals,
            Err(_) => {
                return Ok(Some(admin_observability::DashboardRuntimeSlots {
                    active_accounts,
                    used_slots: None,
                }));
            }
        };
        let used_slots = signals.into_iter().fold(0_u64, |total, signal| {
            total.saturating_add(u64::from(signal.in_flight))
        });
        Ok(Some(admin_observability::DashboardRuntimeSlots {
            active_accounts,
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
