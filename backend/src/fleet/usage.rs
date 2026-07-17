//! 账号调度消费的用量值与持久化端口。

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// 账号用量端口错误。
#[derive(Debug, Error)]
#[error("account usage store operation failed: {message}")]
pub struct AccountUsageStoreError {
    message: String,
}

impl AccountUsageStoreError {
    /// 将 adapter 错误收窄为消费方稳定错误。
    pub fn adapter(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountUsageDelta {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub empty_responses: u64,
    pub image_input_tokens: u64,
    pub image_output_tokens: u64,
    pub image_requests: u64,
    pub image_request_failures: u64,
}

/// fleet 记录一次 Responses 完成结果所需的稳定用量事实。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResponseUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub image_input_tokens: u64,
    pub image_output_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountUsageSnapshot {
    pub request_count: u64,
    pub empty_response_count: u64,
    pub image_input_tokens: u64,
    pub image_output_tokens: u64,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub window_request_count: u64,
    pub window_input_tokens: u64,
    pub window_output_tokens: u64,
    pub window_cached_tokens: u64,
    pub window_image_input_tokens: u64,
    pub window_image_output_tokens: u64,
    pub window_image_request_count: u64,
    pub window_image_request_failed_count: u64,
    pub window_started_at: Option<DateTime<Utc>>,
    pub window_reset_at: Option<DateTime<Utc>>,
    pub limit_window_seconds: Option<u64>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountUsageWindow {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub image_input_tokens: u64,
    pub image_output_tokens: u64,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub reset_at: Option<DateTime<Utc>>,
    pub limit_window_seconds: Option<u64>,
}

#[async_trait]
pub trait AccountUsageStore: Send + Sync + 'static {
    async fn snapshots(
        &self,
        account_ids: &[String],
    ) -> Result<HashMap<String, AccountUsageSnapshot>, AccountUsageStoreError>;

    async fn record_usage_delta(
        &self,
        account_id: &str,
        usage: AccountUsageDelta,
    ) -> Result<(), AccountUsageStoreError>;

    async fn sync_runtime_window(
        &self,
        account_id: &str,
        window: AccountUsageWindow,
    ) -> Result<(), AccountUsageStoreError>;

    async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> Result<(), AccountUsageStoreError>;

    async fn record_request(&self, account_id: &str) -> Result<(), AccountUsageStoreError> {
        self.record_usage_delta(
            account_id,
            AccountUsageDelta {
                requests: 1,
                ..AccountUsageDelta::default()
            },
        )
        .await
    }
}
