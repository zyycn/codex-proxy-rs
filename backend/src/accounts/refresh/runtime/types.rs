use super::*;

pub(super) enum TokenRefreshOutcome {
    Refreshed(Box<Account>),
    StatusUpdated,
    Skipped,
    Failed,
}

/// 单次刷新扫描摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenRefreshSummary {
    /// 扫描账号数。
    pub scanned: usize,
    /// 成功刷新 token 的账号数。
    pub refreshed: usize,
    /// 仅更新状态的账号数。
    pub status_updated: usize,
    /// 无需刷新的账号数。
    pub skipped: usize,
    /// 传输失败账号数。
    pub failed: usize,
}

/// 账号级刷新定时器调度摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTimerSummary {
    /// 扫描账号数。
    pub scanned: usize,
    /// 已调度的未来刷新定时器数。
    pub scheduled: usize,
    /// 已调度的立即刷新定时器数。
    pub immediate: usize,
    /// 无需调度的账号数。
    pub skipped: usize,
    /// 被替换的既有定时器数。
    pub replaced: usize,
}

impl TokenTimerSummary {
    /// 返回本轮调度发生变化的定时器数量。
    pub fn changed(&self) -> usize {
        self.scheduled + self.immediate
    }
}

/// 令牌刷新任务错误。
#[derive(Debug, Error)]
pub enum TokenRefreshServiceError {
    /// 账号存储读取失败。
    #[error("failed to list accounts for token refresh: {0}")]
    AccountStore(#[from] crate::accounts::store::AccountStoreError),
    /// 账号存储写入失败。
    #[error("failed to persist token refresh result: {0}")]
    Store(#[from] PgAccountStoreError),
    /// 刷新租约存储失败。
    #[error("failed to coordinate token refresh lease: {0}")]
    Lease(#[from] RedisRefreshLeaseStoreError),
    /// core 刷新调度失败。
    #[error("token refresh scheduler failed: {0}")]
    Refresh(#[from] RefreshError),
    /// 刷新期间账号不存在。
    #[error("account disappeared during token refresh: {0}")]
    AccountNotFound(String),
}

/// 令牌刷新任务结果。
pub type TokenRefreshServiceResult<T> = Result<T, TokenRefreshServiceError>;

pub(super) fn stored_account_to_refresh_account(stored: StoredAccount) -> Account {
    Account {
        id: stored.id,
        email: stored.email,
        account_id: stored.account_id,
        user_id: stored.user_id,
        label: stored.label,
        plan_type: stored.plan_type,
        access_token: stored.access_token.expose_secret().to_string(),
        refresh_token: stored
            .refresh_token
            .map(|token| token.expose_secret().to_string()),
        access_token_expires_at: stored.access_token_expires_at,
        next_refresh_at: stored.next_refresh_at,
        status: stored.status,
        quota_limit_reached: false,
        quota_verify_required: false,
        quota_cooldown_until: None,
        cloudflare_cooldown_until: None,
        request_count: 0,
        empty_response_count: 0,
        image_input_tokens: 0,
        image_output_tokens: 0,
        image_request_count: 0,
        image_request_failed_count: 0,
        window_request_count: 0,
        window_input_tokens: 0,
        window_output_tokens: 0,
        window_cached_tokens: 0,
        window_image_input_tokens: 0,
        window_image_output_tokens: 0,
        window_image_request_count: 0,
        window_image_request_failed_count: 0,
        window_started_at: None,
        window_reset_at: None,
        limit_window_seconds: None,
        added_at: stored.added_at,
        last_used_at: None,
    }
}
