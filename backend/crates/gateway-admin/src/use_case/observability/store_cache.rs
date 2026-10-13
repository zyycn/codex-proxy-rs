//! 有界统计缓存：身份和完整筛选参与键，删除版本阻止旧数据复活

use crate::model::observability::*;
use crate::ports::store::{
    AdminStoreError, AdminStoreErrorKind, AdminStoreResult, ObservabilityStore,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::{collections::HashMap, future::Future, hash::Hash, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

const CAPACITY: usize = 64;
const TTL: Duration = Duration::from_secs(15);
const MAX_STALE: Duration = Duration::from_secs(60);

type Slot<T> = Arc<Mutex<Option<Cached<T>>>>;
struct Cached<T> {
    value: T,
    created: Instant,
}
struct Entry<T> {
    slot: Slot<T>,
    touched: Instant,
}
struct Cache<K, T> {
    entries: Mutex<HashMap<K, Entry<T>>>,
}
impl<K, T> Default for Cache<K, T> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

trait AnalyticsValue: Clone {
    fn metadata(&mut self) -> &mut AnalyticsMetadata;
}
impl AnalyticsValue for DashboardObservation {
    fn metadata(&mut self) -> &mut AnalyticsMetadata {
        &mut self.analytics
    }
}
impl AnalyticsValue for UsageOverview {
    fn metadata(&mut self) -> &mut AnalyticsMetadata {
        &mut self.analytics
    }
}
impl AnalyticsValue for UsageInsightsObservation {
    fn metadata(&mut self) -> &mut AnalyticsMetadata {
        &mut self.overview.analytics
    }
}
impl AnalyticsValue for DiagnosticsObservation {
    fn metadata(&mut self) -> &mut AnalyticsMetadata {
        &mut self.analytics
    }
}

impl<K: Eq + Hash, T: AnalyticsValue> Cache<K, T> {
    async fn get<F, Fut>(
        &self,
        key: K,
        store: &dyn ObservabilityStore,
        ttl: Duration,
        load: F,
    ) -> AdminStoreResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AdminStoreResult<T>>,
    {
        // 刷新失败后的同键等待也计入预算，避免串行重试把延迟放大为队列长度倍数
        tokio::time::timeout(
            store.query_timeout(),
            self.get_within_budget(key, store, ttl, load),
        )
        .await
        .map_err(|source| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                "observability cache",
                "observability cache execution budget is exhausted",
            )
            .with_source(source)
        })?
    }

    async fn get_within_budget<F, Fut>(
        &self,
        key: K,
        store: &dyn ObservabilityStore,
        ttl: Duration,
        load: F,
    ) -> AdminStoreResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AdminStoreResult<T>>,
    {
        let slot = {
            let mut entries = self.entries.lock().await;
            if !entries.contains_key(&key) && entries.len() >= CAPACITY {
                let oldest = entries
                    .iter()
                    .filter(|(_, entry)| Arc::strong_count(&entry.slot) == 1)
                    .min_by_key(|(_, entry)| entry.touched)
                    .map(|(_, entry)| entry.touched);
                if let Some(oldest) = oldest {
                    entries.retain(|_, entry| entry.touched != oldest);
                }
            }
            if !entries.contains_key(&key) && entries.len() >= CAPACITY {
                drop(entries);
                return load().await;
            }
            let entry = entries.entry(key).or_insert_with(|| Entry {
                slot: Arc::new(Mutex::new(None)),
                touched: Instant::now(),
            });
            entry.touched = Instant::now();
            entry.slot.clone()
        };
        // 同键刷新只执行一次；失败不会缓存成成功，也不延长旧结果寿命
        let mut cached = slot.lock().await;
        // 等锁期间代次可能推进；取得同键锁后再核对，失败时也不能回用已删除事实
        let generation = store.analytics_cache_generation().await?;
        if let Some(entry) = cached.as_mut()
            && entry.value.metadata().generation == generation
            && entry.created.elapsed() < ttl
        {
            return Ok(with_age(entry.value.clone(), false));
        }
        match load().await {
            Ok(value) => {
                *cached = Some(Cached {
                    value: value.clone(),
                    created: Instant::now(),
                });
                Ok(value)
            }
            Err(error) => {
                if ttl == TTL
                    && error.kind() == AdminStoreErrorKind::Unavailable
                    && let Some(entry) = cached.as_mut()
                    && entry.created.elapsed() <= MAX_STALE
                    && Utc::now()
                        .signed_duration_since(entry.value.metadata().generated_at)
                        .to_std()
                        .unwrap_or_default()
                        <= MAX_STALE
                    && entry.value.metadata().state != AnalyticsState::Building
                    && entry.value.metadata().generation
                        == store.analytics_cache_generation().await?
                {
                    return Ok(with_age(entry.value.clone(), true));
                }
                Err(error)
            }
        }
    }
}

fn with_age<T: AnalyticsValue>(mut value: T, stale: bool) -> T {
    let metadata = value.metadata();
    metadata.lag_seconds = metadata.lag_seconds.saturating_add(
        Utc::now()
            .signed_duration_since(metadata.generated_at)
            .num_seconds()
            .max(0) as u64,
    );
    if stale {
        metadata.state = AnalyticsState::Stale;
    }
    value
}

type UsageKey = (TimeRange, UsageFilter);
pub(crate) struct CachedObservabilityStore {
    inner: Arc<dyn ObservabilityStore>,
    dashboard: Cache<DashboardQuery, DashboardObservation>,
    summaries: Cache<UsageKey, UsageOverview>,
    insights: Cache<(UsageKey, Granularity), UsageInsightsObservation>,
    diagnostics: Cache<(UsageKey, DiagnosticDimension, u16), DiagnosticsObservation>,
}
impl CachedObservabilityStore {
    pub(crate) fn new(inner: Arc<dyn ObservabilityStore>) -> Self {
        Self {
            inner,
            dashboard: Cache::default(),
            summaries: Cache::default(),
            insights: Cache::default(),
            diagnostics: Cache::default(),
        }
    }
}

#[async_trait]
impl ObservabilityStore for CachedObservabilityStore {
    fn query_timeout(&self) -> Duration {
        self.inner.query_timeout()
    }

    async fn analytics_cache_generation(&self) -> AdminStoreResult<u64> {
        self.inner.analytics_cache_generation().await
    }
    async fn dashboard_summary(
        &self,
        query: DashboardQuery,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<DashboardObservation> {
        // 复合概览还包含账号容量和最近请求，保持原有两秒实时刷新上限
        self.dashboard
            .get(
                query.clone(),
                self.inner.as_ref(),
                Duration::from_secs(2),
                || self.inner.dashboard_summary(query, observed_at),
            )
            .await
    }
    async fn dashboard_trend(
        &self,
        range: TimeRange,
        granularity: Granularity,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        self.inner.dashboard_trend(range, granularity).await
    }
    async fn usage_trend(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        granularity: Granularity,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        self.inner.usage_trend(range, filter, granularity).await
    }
    async fn usage_insights_observation(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        granularity: Granularity,
    ) -> AdminStoreResult<UsageInsightsObservation> {
        self.insights
            .get(
                ((range, filter.clone()), granularity),
                self.inner.as_ref(),
                TTL,
                || {
                    self.inner
                        .usage_insights_observation(range, filter, granularity)
                },
            )
            .await
    }
    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<UsageOverview> {
        self.summaries
            .get((range, filter.clone()), self.inner.as_ref(), TTL, || {
                self.inner.usage_summary(range, filter)
            })
            .await
    }
    async fn usage_diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
        limit: u16,
    ) -> AdminStoreResult<DiagnosticsObservation> {
        self.diagnostics
            .get(
                ((range, filter.clone()), dimension, limit),
                self.inner.as_ref(),
                TTL,
                || {
                    self.inner
                        .usage_diagnostics(range, filter, dimension, limit)
                },
            )
            .await
    }
    async fn list_usage_records(&self, query: UsageQuery) -> AdminStoreResult<UsagePage> {
        self.inner.list_usage_records(query).await
    }
    async fn usage_record_detail(&self, request_id: &str) -> AdminStoreResult<UsageDetail> {
        self.inner.usage_record_detail(request_id).await
    }
    async fn list_ops_errors(&self, query: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage> {
        self.inner.list_ops_errors(query).await
    }
}
