/// 用量聚合模型、端口与策略服务。
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::accounts::model::AccountUsageDelta;
use crate::codex::protocol::events::TokenUsage;
use crate::infra::json::Page;
use crate::telemetry::usage_store::{SqliteUsageStore, UsageListRecord, UsageSummary};

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

/// 管理端用量统计服务。
#[derive(Clone)]
pub struct AdminUsageService {
    store: SqliteUsageStore,
}

impl AdminUsageService {
    /// 构造服务。
    pub fn new(store: SqliteUsageStore) -> Self {
        Self { store }
    }

    /// 分页列出账号用量。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AdminUsageRecord>, AdminUsageError> {
        let page = self
            .store
            .list_usage(cursor, limit)
            .await
            .map_err(|_| AdminUsageError::List)?;
        Ok(Page {
            items: page.items.into_iter().map(AdminUsageRecord::from).collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// 汇总账号用量。
    pub async fn summary(&self) -> Result<AdminUsageSummary, AdminUsageError> {
        self.store
            .usage_summary()
            .await
            .map(AdminUsageSummary::from)
            .map_err(|_| AdminUsageError::Summary)
    }
}

/// 管理端用量统计错误。
#[derive(Debug, Error)]
pub enum AdminUsageError {
    #[error("failed to list account usage")]
    List,
    #[error("failed to summarize account usage")]
    Summary,
}

/// 管理端账号用量记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUsageRecord {
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
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 管理端账号用量汇总。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminUsageSummary {
    pub account_count: i64,
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
}

impl From<UsageSummary> for AdminUsageSummary {
    fn from(s: UsageSummary) -> Self {
        Self {
            account_count: s.account_count,
            request_count: s.request_count,
            empty_response_count: s.empty_response_count,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            cached_tokens: s.cached_tokens,
            reasoning_tokens: s.reasoning_tokens,
            total_tokens: s.total_tokens,
            image_input_tokens: s.image_input_tokens,
            image_output_tokens: s.image_output_tokens,
            image_request_count: s.image_request_count,
            image_request_failed_count: s.image_request_failed_count,
        }
    }
}

impl From<UsageListRecord> for AdminUsageRecord {
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
            last_used_at: usage.last_used_at,
        }
    }
}
