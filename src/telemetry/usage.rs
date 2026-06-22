/// Usage record for API responses.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageRecord {
    pub request_id: String,
    pub account_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}
/// 用量聚合模型、端口与策略服务。
use crate::accounts::store::UsageSummary;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::accounts::model::AccountUsageDelta;
use crate::codex::protocol::events::TokenUsage;

/// 用量聚合窗口。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    /// 窗口开始时间。
    pub started_at: DateTime<Utc>,
    /// 请求数。
    pub request_count: u64,
    /// 输入 token 数。
    pub input_tokens: u64,
    /// 输出 token 数。
    pub output_tokens: u64,
}

/// 用量快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    /// 账号 ID。
    pub account_id: String,
    /// 当前窗口。
    pub window: UsageWindow,
}

/// 用量存储错误。
#[derive(Debug, Error)]
pub enum UsageStoreError {
    /// 底层存储失败。
    #[error("usage store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 用量存储结果。
pub type UsageStoreResult<T> = Result<T, UsageStoreError>;

/// 用量存储端口。
#[async_trait]
pub trait UsageStore: Send + Sync + 'static {
    /// 写入用量快照。
    async fn record_snapshot(&self, snapshot: &UsageSnapshot) -> UsageStoreResult<()>;
}

/// 用量聚合服务。
#[derive(Debug, Clone, Default)]
pub struct UsageService;

impl UsageService {
    /// 将 token 增量累加到窗口。
    pub fn add_tokens(window: &mut UsageWindow, input_tokens: u64, output_tokens: u64) {
        window.request_count += 1;
        window.input_tokens += input_tokens;
        window.output_tokens += output_tokens;
    }

    /// 将标准化 token 用量转换为账号持久化用量增量。
    pub fn account_delta_from_token_usage(usage: TokenUsage) -> AccountUsageDelta {
        AccountUsageDelta {
            requests: 0,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            reasoning_tokens: 0,
            total_tokens: usage.input_tokens + usage.output_tokens + usage.cached_tokens,
            empty_responses: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            image_requests: 0,
            image_request_failures: 0,
        }
    }
}

/// Admin usage summary.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdminUsageSummary {
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_reasoning_tokens: u64,
}

impl From<UsageSummary> for AdminUsageSummary {
    fn from(s: UsageSummary) -> Self {
        Self {
            total_requests: s.request_count as u64,
            total_input_tokens: s.input_tokens as u64,
            total_output_tokens: s.output_tokens as u64,
            total_cached_tokens: s.cached_tokens as u64,
            total_reasoning_tokens: s.reasoning_tokens as u64,
        }
    }
}
