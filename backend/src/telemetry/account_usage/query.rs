//! 用量聚合模型、端口与策略服务。
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::infra::format::nonnegative_i64_to_u64;
use crate::telemetry::{
    account_usage::store::{PgAccountUsageStore, UsageListRecord},
    billing,
    buckets::query::{
        ModelBucketUsage, ModelUsageWindow, PgRequestBucketQuery, UsageBucketTotals,
        UsageBucketWindow, UsageTimeBucketRecord,
    },
};

/// 管理端用量统计服务。
#[derive(Clone)]
pub struct AccountUsageQueryService {
    store: PgAccountUsageStore,
    buckets: PgRequestBucketQuery,
}

impl AccountUsageQueryService {
    /// 构造服务。
    pub fn new(store: PgAccountUsageStore) -> Self {
        Self {
            buckets: PgRequestBucketQuery::new(store.pool().clone()),
            store,
        }
    }

    /// 按账号 ID 批量读取账号用量。
    pub async fn list_by_account_ids(
        &self,
        account_ids: &[String],
    ) -> Result<Vec<AccountUsageRecord>, AccountUsageQueryError> {
        self.store
            .list_usage_by_account_ids(account_ids)
            .await
            .map(|records| records.into_iter().map(AccountUsageRecord::from).collect())
            .map_err(|_| AccountUsageQueryError::List)
    }

    /// 汇总请求时间桶保留期内的全局用量与计费。
    pub async fn retained_summary(&self) -> Result<RetainedUsageSummary, AccountUsageQueryError> {
        let buckets = self
            .buckets
            .retained_usage_buckets()
            .await
            .map_err(|_| AccountUsageQueryError::RetainedSummary)?;
        let mut summary = RetainedUsageSummary::default();
        for bucket in buckets {
            summary.billing_amount_usd += billing::calculate_billing_amount(
                nonnegative_i64_to_u64(bucket.totals.input_tokens),
                nonnegative_i64_to_u64(bucket.totals.output_tokens),
                nonnegative_i64_to_u64(bucket.totals.cached_tokens),
                nonnegative_i64_to_u64(bucket.totals.cache_write_tokens),
                &bucket.model,
                bucket.service_tier.as_deref(),
            );
            summary.totals.add(bucket.totals);
        }
        Ok(summary)
    }

    /// 列出指定时间范围内的聚合时间桶。
    pub async fn time_buckets(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<AccountUsageTimeBucket>, AccountUsageQueryError> {
        self.buckets
            .list(start, end)
            .await
            .map(|records| {
                records
                    .into_iter()
                    .map(AccountUsageTimeBucket::from)
                    .collect()
            })
            .map_err(|_| AccountUsageQueryError::TimeBuckets)
    }

    pub async fn usage_by_windows(
        &self,
        windows: &[UsageBucketWindow],
    ) -> Result<HashMap<String, HashMap<String, UsageBucketTotals>>, AccountUsageQueryError> {
        self.buckets
            .usage_by_windows(windows)
            .await
            .map_err(|_| AccountUsageQueryError::TimeBuckets)
    }

    pub async fn model_usage_by_windows(
        &self,
        windows: &[ModelUsageWindow],
    ) -> Result<Vec<ModelBucketUsage>, AccountUsageQueryError> {
        self.buckets
            .model_usage_by_windows(windows)
            .await
            .map_err(|_| AccountUsageQueryError::TimeBuckets)
    }
}

/// 管理端用量统计错误。
#[derive(Debug, Error)]
pub enum AccountUsageQueryError {
    #[error("failed to list account usage")]
    List,
    #[error("failed to summarize retained usage")]
    RetainedSummary,
    #[error("failed to list usage time buckets")]
    TimeBuckets,
}

/// 请求时间桶保留期内的全局用量与计费汇总。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RetainedUsageSummary {
    pub totals: UsageBucketTotals,
    pub billing_amount_usd: f64,
}

/// 管理端账号用量记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageRecord {
    pub account_id: String,
    pub email: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
    pub image_request_count: i64,
    pub image_request_failed_count: i64,
    pub window_request_count: i64,
    pub window_input_tokens: i64,
    pub window_output_tokens: i64,
    pub window_cached_tokens: i64,
    pub window_started_at: Option<DateTime<Utc>>,
    pub window_reset_at: Option<DateTime<Utc>>,
    pub limit_window_seconds: Option<u64>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 管理端时间桶用量记录。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsageTimeBucket {
    pub bucket_start: DateTime<Utc>,
    pub model: String,
    pub service_tier: Option<String>,
    pub request_count: i64,
    pub error_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: i64,
    pub first_token_latency_sum: i64,
    pub first_token_latency_count: i64,
    pub latency_sum: i64,
    pub latency_count: i64,
    pub max_latency_ms: i64,
    pub min_latency_ms: i64,
}

impl From<UsageTimeBucketRecord> for AccountUsageTimeBucket {
    fn from(record: UsageTimeBucketRecord) -> Self {
        Self {
            bucket_start: record.bucket_start,
            model: record.model,
            service_tier: record.service_tier,
            request_count: record.request_count,
            error_count: record.error_count,
            input_tokens: record.input_tokens,
            output_tokens: record.output_tokens,
            cached_tokens: record.cached_tokens,
            cache_write_tokens: record.cache_write_tokens,
            first_token_latency_sum: record.first_token_latency_sum,
            first_token_latency_count: record.first_token_latency_count,
            latency_sum: record.latency_sum,
            latency_count: record.latency_count,
            max_latency_ms: record.max_latency_ms,
            min_latency_ms: record.min_latency_ms,
        }
    }
}

impl From<UsageListRecord> for AccountUsageRecord {
    fn from(usage: UsageListRecord) -> Self {
        Self {
            account_id: usage.account_id,
            email: usage.email,
            label: usage.label,
            plan_type: usage.plan_type,
            request_count: usage.request_count,
            empty_response_count: usage.empty_response_count,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            total_tokens: usage.total_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_request_count: usage.image_request_count,
            image_request_failed_count: usage.image_request_failed_count,
            window_request_count: usage.window_request_count,
            window_input_tokens: usage.window_input_tokens,
            window_output_tokens: usage.window_output_tokens,
            window_cached_tokens: usage.window_cached_tokens,
            window_started_at: usage.window_started_at,
            window_reset_at: usage.window_reset_at,
            limit_window_seconds: usage.limit_window_seconds,
            last_used_at: usage.last_used_at,
        }
    }
}
