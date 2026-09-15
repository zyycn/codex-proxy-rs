//! Pg 观测 adapter：实现 `ObservabilityRepository` 与 `AdminObservabilityStore`。

use futures::{StreamExt, TryStreamExt, stream::BoxStream};
use gateway_admin::ports::store::UsageCalculatedBillingStream;
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
    query_budget: ObservabilityQueryBudget,
}

impl PgObservabilityRepository {
    #[must_use]
    pub fn new(
        pool: PgPool,
        cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
        query_budget: ObservabilityQueryBudget,
    ) -> Self {
        Self {
            pool,
            cooldowns,
            query_budget,
        }
    }

    /// 从账号事实和当前冷却一次性派生 Dashboard 五态；不在 SQL 中复制状态机。
    async fn account_status_snapshot(
        &self,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<(ProviderAccountMetrics, Vec<ProviderAccountSummary>)> {
        let repository = PgProviderAccountRepository::new(self.pool.clone());
        let accounts = self
            .query_budget
            .run(
                "load dashboard provider accounts",
                repository.list_provider_accounts(None, true),
            )
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
                gateway_core::account::AccountStatus::Normal => {
                    metrics.normal = metrics.normal.saturating_add(1);
                    normal_accounts.push(account.clone());
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
        query_budget: ObservabilityQueryBudget,
    ) -> Self {
        Self {
            repository: PgObservabilityRepository::new(pool, cooldowns, query_budget),
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
        let account_usage_query =
            ProviderAccountUsageQuery::recent(range, DASHBOARD_ACCOUNT_LIMIT)?
                .with_hourly_request_buckets()?;
        let recent_query = UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                outcome: Some("succeeded".to_owned()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10)?,
        };
        // 每条 SQL 独立取一个全局观测槽位，避免整包预留造成队头阻塞。
        let (totals, (provider_accounts, _)) = futures::try_join!(
            self.query_budget.run(
                "load dashboard lifetime totals",
                dashboard_totals(&self.pool)
            ),
            self.account_status_snapshot(observed_at),
        )?;
        let (trend, account_usage, recent_requests) = futures::try_join!(
            self.query_budget.run(
                "load dashboard request trend",
                dashboard_request_metric_series(&self.pool, range, &filter),
            ),
            self.query_budget.run(
                "load dashboard account usage",
                provider_account_usage(&self.pool, account_usage_query),
            ),
            self.query_budget
                .run("load dashboard recent requests", async {
                    list_usage_record_items(&self.pool, &recent_query).await
                }),
        )?;
        Ok(DashboardObservation {
            range,
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
        self.query_budget
            .run(
                "load dashboard request trend",
                dashboard_request_metric_series(&self.pool, range, &UsageRecordFilter::default()),
            )
            .await
    }

    async fn usage_trend(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        self.query_budget
            .run(
                "load usage request trend",
                request_metric_series(&self.pool, range, &filter),
            )
            .await
    }

    fn usage_calculated_billing_facts(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> BoxStream<'_, StoreResult<CalculatedUsageBillingFact>> {
        self.query_budget.run_stream(
            "load calculated usage billing facts",
            calculated_usage_billing_facts(&self.pool, range, filter),
        )
    }

    async fn provider_account_usage(
        &self,
        query: ProviderAccountUsageQuery,
    ) -> StoreResult<Vec<ProviderAccountUsageObservation>> {
        self.query_budget
            .run(
                "load provider account usage",
                provider_account_usage(&self.pool, query),
            )
            .await
    }

    async fn list_usage_records(&self, query: UsageRecordQuery) -> StoreResult<UsageRecordPage> {
        self.query_budget
            .run("list usage records", list_usage_records(&self.pool, query))
            .await
    }

    async fn usage_record_detail(&self, request_id: &str) -> StoreResult<UsageRecordDetail> {
        self.query_budget
            .run(
                "load usage record detail",
                usage_record_detail(&self.pool, request_id),
            )
            .await
    }

    async fn usage_summary(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<UsageOverview> {
        filter.validate()?;
        let requests = self
            .query_budget
            .run(
                "load usage request metrics",
                request_metrics(&self.pool, range, &filter),
            )
            .await?;
        let attempts = self
            .query_budget
            .run(
                "load usage attempt metrics",
                attempt_metrics(&self.pool, range, &filter),
            )
            .await?;
        let providers = self
            .query_budget
            .run(
                "load usage provider observations",
                provider_observations(&self.pool, range, &filter),
            )
            .await?;
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
        self.query_budget
            .run(
                "load usage diagnostics",
                usage_diagnostics(&self.pool, range, &filter, dimension),
            )
            .await
    }

    async fn list_ops_errors(&self, query: OpsErrorQuery) -> StoreResult<OpsErrorPage> {
        self.query_budget
            .run("list ops errors", list_ops_errors(&self.pool, query))
            .await
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

    fn usage_calculated_billing_facts(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
    ) -> UsageCalculatedBillingStream<'_> {
        let range = match store_range(range) {
            Ok(range) => range,
            Err(error) => return Box::pin(futures::stream::once(async { Err(error) })),
        };
        self.repository
            .usage_calculated_billing_facts(range, store_usage_filter(filter))
            .map_err(observability_error)
            .map(|fact| fact.and_then(admin_calculated_usage_billing_fact))
            .boxed()
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
