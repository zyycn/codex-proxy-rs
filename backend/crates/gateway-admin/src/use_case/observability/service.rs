//! 观测查询与账号信息补充的用例协调

use super::billing::usd_cost;
use super::dashboard::{dashboard_period_metrics, health_timeline_at, trend};
use super::insights::{insights_from_observation, select_usage_metric};
use super::metrics::{average, rate_or_zero};
use crate::model::AdminError;
use crate::model::observability::{
    DashboardAccountUsage, DashboardCapacity, DashboardQuery, DashboardResult, DiagnosticDimension,
    DiagnosticsItem, DiagnosticsResult, Granularity, OpsErrorPage, OpsErrorQuery,
    ProviderBillingInput, RequestHealth, TimeRange, Trend, TrendKind, UsageBilling, UsageDetail,
    UsageFilter, UsageInsight, UsageInsightMetric, UsageInsights, UsagePage, UsageQuery,
    UsageSummary,
};
use crate::model::provider_credentials::ProviderQuotaRequest;
use crate::ports::provider::ProviderAdminRegistry;
use crate::ports::store::{AccountStore, ObservabilityStore, SettingsStore};
use crate::use_case::map_store_error;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use futures::StreamExt as _;
use gateway_core::account::ProviderAccountId;
use gateway_core::routing::ProviderKind;
use std::collections::HashMap;
use std::sync::Arc;

/// API 消费的观测控制面服务
#[async_trait]
pub trait ObservabilityService: Send + Sync {
    async fn dashboard_summary(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<DashboardResult, AdminError>;
    async fn dashboard_trend(&self, range: TimeRange, kind: TrendKind)
    -> Result<Trend, AdminError>;
    async fn usage_records(&self, query: UsageQuery) -> Result<UsagePage, AdminError>;
    async fn usage_record_detail(&self, request_id: &str) -> Result<UsageDetail, AdminError>;
    async fn client_key_statistics(
        &self,
        range: TimeRange,
        id: &gateway_core::policy::ClientApiKeyId,
        query: crate::model::observability::KeyUsageMetric,
    ) -> Result<crate::model::observability::KeyStatisticsOverview, AdminError>;

    async fn client_key_models(
        &self,
        range: TimeRange,
        id: &gateway_core::policy::ClientApiKeyId,
    ) -> Result<Vec<crate::model::observability::KeyModelRow>, AdminError>;

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageSummary, AdminError>;
    async fn usage_insights(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageInsights, AdminError>;
    async fn usage_metric(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        metric: UsageInsightMetric,
    ) -> Result<UsageInsight, AdminError>;
    async fn diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
    ) -> Result<DiagnosticsResult, AdminError>;
    async fn ops_errors(&self, query: OpsErrorQuery) -> Result<OpsErrorPage, AdminError>;
    async fn export_diagnostics(
        &self,
        target: crate::model::diagnostics::DiagnosticTarget,
    ) -> Result<crate::model::diagnostics::DiagnosticBundle, AdminError>;
}

pub(crate) struct DefaultObservabilityService {
    pub(super) timezone: gateway_core::time::DeploymentTimeZone,
    pub(super) store: Arc<dyn ObservabilityStore>,
    pub(super) accounts: Arc<dyn AccountStore>,
    pub(super) settings: Arc<dyn SettingsStore>,
    pub(super) providers: ProviderAdminRegistry,
    pub(super) system: Arc<dyn crate::use_case::system::SystemService>,
    pub(super) upstream_info: Option<Arc<dyn crate::ports::peer::PeerUpstreamService>>,
}

impl DefaultObservabilityService {
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn ObservabilityStore>,
        accounts: Arc<dyn AccountStore>,
        settings: Arc<dyn SettingsStore>,
        providers: ProviderAdminRegistry,
        timezone: gateway_core::time::DeploymentTimeZone,
        system: Arc<dyn crate::use_case::system::SystemService>,
        upstream_info: Option<Arc<dyn crate::ports::peer::PeerUpstreamService>>,
    ) -> Self {
        Self {
            timezone,
            store,
            accounts,
            settings,
            providers,
            system,
            upstream_info,
        }
    }

    async fn load_dashboard_summary(
        &self,
        range: TimeRange,
    ) -> Result<DashboardResult, AdminError> {
        let observed_at = Utc::now();
        let query = DashboardQuery::new(range, self.timezone)
            .map_err(|_| AdminError::internal("时间超出支持范围"))?;
        let today_start = query.yesterday_range.end;
        let (mut observation, settings) = futures::try_join!(
            self.store.dashboard_summary(query, observed_at),
            self.settings.load_runtime_settings(),
        )
        .map_err(|error| map_store_error(error, "dashboard"))?;
        let runtime_slots = observation.runtime_slots.as_ref();
        self.enrich_list_records(&mut observation.recent_requests)
            .await;
        self.enrich_dashboard_quotas(&mut observation.account_usage)
            .await;
        for account in &mut observation.account_usage {
            account.plan_type_display = self
                .providers
                .plan_type_display(&account.provider_kind, account.plan_type.as_deref());
        }
        let today = dashboard_period_metrics(
            observation
                .trend
                .iter()
                .filter(|point| {
                    point.bucket_start >= today_start && point.bucket_start < observation.range.end
                })
                .map(|point| &point.metrics),
        );
        let yesterday = dashboard_period_metrics(std::iter::once(&observation.yesterday));
        let total_billing_usd = observation.totals.billing_usd.clone();
        let total_cached_token_rate = rate_or_zero(
            observation.totals.cached_input_tokens,
            observation.totals.input_tokens,
        );
        let (first_token_latency_sum_ms, first_token_latency_count) = observation
            .trend
            .iter()
            .fold((0_u64, 0_u64), |(sum, count), point| {
                (
                    sum.saturating_add(point.metrics.first_token_latency_sum_ms),
                    count.saturating_add(point.metrics.first_token_latency_count),
                )
            });
        let average_first_token_latency_ms =
            average(first_token_latency_sum_ms, first_token_latency_count);
        let trend = trend(
            TrendKind::Usage,
            observation.trend.clone(),
            observation.analytics.clone(),
        )?;
        let health_timeline =
            health_timeline_at(&observation.trend, observation.range.end, self.timezone)?;
        let wire_profiles = self
            .providers
            .dashboard_wire_profiles(&settings.request_profiles);
        let max_concurrent_per_account = u64::from(settings.values.max_concurrent_per_account);
        let (mut inherited_accounts, mut overridden_slots) = runtime_slots
            .map_or((observation.provider_accounts.normal, 0), |slots| {
                (slots.inherited_accounts, slots.overridden_slots)
            });
        if let (Some(peer), Some(slots)) = (&self.upstream_info, runtime_slots) {
            let now = std::time::SystemTime::now();
            for version in &slots.inherited_versions {
                if let Some(limit) = peer.observed_concurrency_limit(version, now) {
                    inherited_accounts -= 1;
                    overridden_slots = overridden_slots.saturating_add(u64::from(limit.get()));
                }
            }
        }
        let total_slots = (max_concurrent_per_account > 0 || inherited_accounts == 0).then(|| {
            inherited_accounts
                .saturating_mul(max_concurrent_per_account)
                .saturating_add(overridden_slots)
        });
        let used_slots = runtime_slots.and_then(|slots| slots.used_slots);
        Ok(DashboardResult {
            capacity: DashboardCapacity {
                max_concurrent_per_account,
                total_slots,
                used_slots,
                available_slots: used_slots
                    .zip(total_slots)
                    .map(|(used, total)| total.saturating_sub(used)),
            },
            rotation_strategy: settings.rotation_strategy,
            observation,
            today,
            yesterday,
            total_billing_usd,
            total_cached_token_rate,
            average_first_token_latency_ms,
            trend,
            health_timeline,
            wire_profiles,
        })
    }
}

#[async_trait]
impl ObservabilityService for DefaultObservabilityService {
    async fn export_diagnostics(
        &self,
        target: crate::model::diagnostics::DiagnosticTarget,
    ) -> Result<crate::model::diagnostics::DiagnosticBundle, AdminError> {
        self.collect_diagnostics(target).await
    }

    async fn dashboard_summary(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<DashboardResult, AdminError> {
        let mut result = self.load_dashboard_summary(range).await?;
        result.trend = trend(
            kind,
            result.observation.trend.clone(),
            result.observation.analytics.clone(),
        )?;
        Ok(result)
    }

    async fn dashboard_trend(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<Trend, AdminError> {
        let observation = self
            .store
            .usage_insights_observation(
                range,
                UsageFilter::default(),
                Granularity::for_range(range),
                false,
            )
            .await
            .map_err(|error| map_store_error(error, "dashboard trend"))?;
        trend(kind, observation.trend, observation.overview.analytics)
    }

    async fn usage_records(&self, query: UsageQuery) -> Result<UsagePage, AdminError> {
        let mut page = self
            .store
            .list_usage_records(query)
            .await
            .map_err(|error| map_store_error(error, "usage records"))?;
        self.enrich_list_records(&mut page.items).await;
        Ok(page)
    }

    async fn usage_record_detail(&self, request_id: &str) -> Result<UsageDetail, AdminError> {
        if request_id.trim().is_empty() {
            return Err(AdminError::invalid("用量记录 ID 不能为空"));
        }
        let mut detail = self
            .store
            .usage_record_detail(request_id)
            .await
            .map_err(|error| map_store_error(error, "usage record"))?;
        self.enrich_detail_billing(std::slice::from_mut(&mut detail.request));
        Ok(detail)
    }

    async fn client_key_statistics(
        &self,
        range: TimeRange,
        id: &gateway_core::policy::ClientApiKeyId,
        query: crate::model::observability::KeyUsageMetric,
    ) -> Result<crate::model::observability::KeyStatisticsOverview, AdminError> {
        self.store
            .client_key_statistics(range, id, query)
            .await
            .map_err(|error| map_store_error(error, "client key statistics"))
    }

    async fn client_key_models(
        &self,
        range: TimeRange,
        id: &gateway_core::policy::ClientApiKeyId,
    ) -> Result<Vec<crate::model::observability::KeyModelRow>, AdminError> {
        self.store
            .client_key_models(range, id)
            .await
            .map_err(|error| map_store_error(error, "client key statistics"))
    }

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageSummary, AdminError> {
        let observation = self
            .store
            .usage_summary(range, filter)
            .await
            .map_err(|error| map_store_error(error, "usage summary"))?;
        let average_latency_ms = average(
            observation.requests.latency_sum_ms,
            observation.requests.latency_count,
        );
        Ok(UsageSummary {
            observation,
            average_latency_ms,
        })
    }

    async fn usage_insights(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageInsights, AdminError> {
        let granularity = Granularity::for_range(range);
        let observation = self
            .store
            .usage_insights_observation(range, filter, granularity, true)
            .await
            .map_err(|error| map_store_error(error, "usage insights"))?;
        insights_from_observation(observation)
    }

    async fn usage_metric(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        metric: UsageInsightMetric,
    ) -> Result<UsageInsight, AdminError> {
        let observation = self
            .store
            .usage_metric_observation(range, filter, Granularity::for_range(range), metric)
            .await
            .map_err(|error| map_store_error(error, "usage metric"))?;
        let insights = insights_from_observation(observation)?;
        Ok(select_usage_metric(insights, metric))
    }

    async fn diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
    ) -> Result<DiagnosticsResult, AdminError> {
        let observation = self
            .store
            .usage_diagnostics(range, filter, dimension, 100)
            .await
            .map_err(|error| map_store_error(error, "usage diagnostics"))?;
        let peers = if dimension == DiagnosticDimension::Account {
            self.current_upstream_peers(observation.items.iter().map(|item| item.key.as_str()))
                .await
        } else {
            HashMap::new()
        };
        let items = observation
            .items
            .into_iter()
            .map(|item| {
                let health = RequestHealth::from_counts(
                    item.success_count,
                    item.failure_count,
                    item.cancelled_count,
                    item.incomplete_count,
                    item.caller_error_count,
                );
                let non_completion_count =
                    item.cancelled_count.saturating_add(item.incomplete_count);
                let non_completion_rate = rate_or_zero(non_completion_count, item.request_count);
                let retry_rate = rate_or_zero(item.retried_request_count, item.request_count);
                DiagnosticsItem {
                    account_upstream_peer: peers.get(&item.key).copied(),
                    key: item.key,
                    name: item.name,
                    account_authentication_kind: item.account_authentication_kind,
                    account_plan_type_display: item.account_provider_kind.as_deref().and_then(
                        |provider| {
                            self.providers
                                .plan_type_display(provider, item.account_plan_type.as_deref())
                        },
                    ),
                    account_plan_type: item.account_plan_type,
                    request_count: item.request_count,
                    success_count: item.success_count,
                    error_count: health.non_success_requests(),
                    error_rate: health.error_ratio(),
                    request_share: rate_or_zero(
                        item.request_count,
                        observation.total_request_count,
                    ),
                    average_latency_ms: item.average_latency_ms,
                    latency_p95_ms: item.latency_p95_ms,
                    first_token_p95_ms: item.first_token_p95_ms,
                    non_completion_count,
                    non_completion_rate,
                    retry_count: item.retry_count,
                    retry_rate,
                    estimated_cost: usd_cost(&item.costs),
                    attempt_count: item.attempt_count,
                    total_tokens: item.total_tokens,
                }
            })
            .collect::<Vec<_>>();
        Ok(DiagnosticsResult {
            analytics: observation.analytics,
            dimension,
            items,
        })
    }

    async fn ops_errors(&self, query: OpsErrorQuery) -> Result<OpsErrorPage, AdminError> {
        let mut page = self
            .store
            .list_ops_errors(query)
            .await
            .map_err(|error| map_store_error(error, "operations errors"))?;
        for error in &mut page.items {
            error.provider_account_plan_type_display =
                error.provider_kind.as_deref().and_then(|provider| {
                    self.providers
                        .plan_type_display(provider, error.provider_account_plan_type.as_deref())
                });
        }
        Ok(page)
    }
}

impl DefaultObservabilityService {
    async fn current_upstream_peers<'a>(
        &self,
        ids: impl Iterator<Item = &'a str>,
    ) -> HashMap<String, bool> {
        let Some(peer) = self.upstream_info.as_deref() else {
            return HashMap::new();
        };
        let ids = ids
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        // 同一页重复账号只读取一次摘要，单个账号不存在或读取失败不阻断历史记录
        futures::stream::iter(ids)
            .map(|id: String| async move {
                let account_id = ProviderAccountId::new(id.clone()).ok()?;
                let upstream = peer.read_summary(&account_id).await.ok()??.upstream?;
                Some((id, upstream.discovered && upstream.observed_at.is_some()))
            })
            .buffer_unordered(8)
            .filter_map(|value| async move { value })
            .collect()
            .await
    }

    async fn enrich_dashboard_quotas(&self, accounts: &mut [DashboardAccountUsage]) {
        if accounts.is_empty() {
            return;
        }
        let now = Utc::now();
        let ids = accounts
            .iter()
            .map(|account| account.account_id.clone())
            .collect::<Vec<_>>();
        let mut rolling_usage = self
            .accounts
            .load_account_usage(
                TimeRange {
                    start: now - Duration::hours(24),
                    end: now,
                },
                &ids,
            )
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|usage| (usage.account_id.clone(), usage))
            .collect::<HashMap<_, _>>();
        let providers = self.providers.clone();
        let peer = self.upstream_info.as_deref();
        let quota_reads = accounts.iter().map(|account| {
            let rolling_usage = rolling_usage.remove(&account.account_id);
            let account_id = ProviderAccountId::new(account.account_id.clone()).ok();
            let provider = ProviderKind::new(account.provider_kind.clone())
                .ok()
                .and_then(|kind| providers.require(&kind).ok());
            async move {
                let (Some(account_id), Some(provider)) = (account_id, provider) else {
                    return None;
                };
                if let Some(peer) = peer
                    && let Ok(Some(quota)) = peer.read_summary(&account_id).await
                {
                    return Some(quota);
                }
                provider
                    .quota(ProviderQuotaRequest {
                        account_id,
                        refresh: false,
                        rolling_usage,
                    })
                    .await
                    .ok()
            }
        });
        let quotas = futures::future::join_all(quota_reads).await;
        for (account, quota) in accounts.iter_mut().zip(quotas) {
            if let Some(mut quota) = quota {
                quota.fill_missing_plan_type(&mut account.plan_type);
                quota.apply_limit_reached_display();
                account.quota_used_percent = quota.representative_used_percent();
                account.quota_window = quota.representative_window().cloned();
                account.upstream = quota.upstream;
            }
        }
    }

    /// 补充当前套餐展示名称，逐条尽力把可校验的总额升级为完整分解；单条脏数据（非法 Provider kind、
    /// 不支持的来源或费用规则失败）只保留该条已存的总额，不影响整页返回
    async fn enrich_list_records(
        &self,
        records: &mut [crate::model::observability::UsageListRecord],
    ) {
        let peers = self
            .current_upstream_peers(
                records
                    .iter()
                    .filter_map(|record| record.provider_account_ref.as_deref()),
            )
            .await;
        for record in records {
            record.provider_account_upstream_peer = record
                .provider_account_ref
                .as_ref()
                .and_then(|id| peers.get(id).copied());
            record.provider_account_plan_type_display =
                record.provider_kind.as_deref().and_then(|provider| {
                    self.providers
                        .plan_type_display(provider, record.provider_account_plan_type.as_deref())
                });
            let Some(UsageBilling::Total { source, total }) = record.billing.as_ref() else {
                continue;
            };
            if !matches!(source.as_str(), "calculated" | "provider_reported") {
                continue;
            }
            let (Some(provider), Some(upstream_model_id)) = (
                record.provider_kind.as_deref(),
                record.upstream_model_id.as_ref(),
            ) else {
                continue;
            };
            let Ok(provider_kind) = ProviderKind::new(provider.to_owned()) else {
                continue;
            };
            let input = ProviderBillingInput {
                upstream_model_id: upstream_model_id.clone(),
                service_tier: record.service_tier.clone(),
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cached_input_tokens: record.cached_input_tokens,
                cache_write_input_tokens: record.cache_write_input_tokens,
                total: total.clone(),
            };
            if let Ok(Some(breakdown)) = self.providers.calculated_billing(&provider_kind, &input) {
                record.billing = Some(UsageBilling::Calculated(Box::new(breakdown)));
            }
        }
    }

    fn enrich_detail_billing(&self, records: &mut [crate::model::observability::UsageRecord]) {
        for record in records {
            let Some(UsageBilling::Total { source, total }) = record.billing.as_ref() else {
                continue;
            };
            if !matches!(source.as_str(), "calculated" | "provider_reported") {
                continue;
            }
            let (Some(provider), Some(upstream_model_id)) = (
                record.provider_kind.as_deref(),
                record.upstream_model_id.as_ref(),
            ) else {
                continue;
            };
            let Ok(provider_kind) = ProviderKind::new(provider.to_owned()) else {
                continue;
            };
            let input = ProviderBillingInput {
                upstream_model_id: upstream_model_id.clone(),
                service_tier: record.service_tier.clone(),
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cached_input_tokens: record.cached_input_tokens,
                cache_write_input_tokens: record.cache_write_input_tokens,
                total: total.clone(),
            };
            if let Ok(Some(breakdown)) = self.providers.calculated_billing(&provider_kind, &input) {
                record.billing = Some(UsageBilling::Calculated(Box::new(breakdown)));
            }
        }
    }
}
