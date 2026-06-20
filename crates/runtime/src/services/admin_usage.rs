use super::*;

/// 管理端用量服务。
#[derive(Clone)]
pub struct AdminUsageService {
    store: SqliteAccountStore,
}

impl AdminUsageService {
    /// 构造管理端用量服务。
    pub fn new(store: SqliteAccountStore) -> Self {
        Self { store }
    }

    /// 分页列出账号用量统计。
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

    /// 汇总账号用量统计。
    pub async fn summary(&self) -> Result<AdminUsageSummary, AdminUsageError> {
        self.store
            .usage_summary()
            .await
            .map(AdminUsageSummary::from)
            .map_err(|_| AdminUsageError::Summary)
    }
}

/// 管理端用量错误。
#[derive(Debug, Error)]
pub enum AdminUsageError {
    /// 列表失败。
    #[error("failed to list account usage")]
    List,
    /// 汇总失败。
    #[error("failed to summarize account usage")]
    Summary,
}

/// 管理端用量记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUsageRecord {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 请求数。
    pub request_count: i64,
    /// 空响应数。
    pub empty_response_count: i64,
    /// 输入 token 数。
    pub input_tokens: i64,
    /// 输出 token 数。
    pub output_tokens: i64,
    /// 缓存 token 数。
    pub cached_tokens: i64,
    /// reasoning token 数。
    pub reasoning_tokens: i64,
    /// 上游返回的总 token 数。
    pub total_tokens: i64,
    /// 图片输入 token 数。
    pub image_input_tokens: i64,
    /// 图片输出 token 数。
    pub image_output_tokens: i64,
    /// 图片请求数。
    pub image_request_count: i64,
    /// 图片请求失败数。
    pub image_request_failed_count: i64,
    /// 最近使用时间。
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 管理端用量汇总。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminUsageSummary {
    /// 有用量记录的账号数。
    pub account_count: i64,
    /// 请求总数。
    pub request_count: i64,
    /// 空响应总数。
    pub empty_response_count: i64,
    /// 输入 token 总数。
    pub input_tokens: i64,
    /// 输出 token 总数。
    pub output_tokens: i64,
    /// 缓存 token 总数。
    pub cached_tokens: i64,
    /// reasoning token 总数。
    pub reasoning_tokens: i64,
    /// 上游返回 token 总数。
    pub total_tokens: i64,
    /// 图片输入 token 总数。
    pub image_input_tokens: i64,
    /// 图片输出 token 总数。
    pub image_output_tokens: i64,
    /// 图片请求总数。
    pub image_request_count: i64,
    /// 图片请求失败总数。
    pub image_request_failed_count: i64,
}

impl From<AccountUsageListRecord> for AdminUsageRecord {
    fn from(usage: AccountUsageListRecord) -> Self {
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

impl From<AccountUsageSummary> for AdminUsageSummary {
    fn from(summary: AccountUsageSummary) -> Self {
        Self {
            account_count: summary.account_count,
            request_count: summary.request_count,
            empty_response_count: summary.empty_response_count,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cached_tokens: summary.cached_tokens,
            reasoning_tokens: summary.reasoning_tokens,
            total_tokens: summary.total_tokens,
            image_input_tokens: summary.image_input_tokens,
            image_output_tokens: summary.image_output_tokens,
            image_request_count: summary.image_request_count,
            image_request_failed_count: summary.image_request_failed_count,
        }
    }
}
