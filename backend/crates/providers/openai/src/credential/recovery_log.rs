//! 成功取得 OAuth token 后写入独立恢复日志的记录。
//!
//! 此日志会刻意保存原始 AT/RT，避免后续数据库写入或校验失败时，已成功
//! 交换的账号无法恢复。记录独立落盘，但沿用普通结构化日志的滚动和清理策略。

const OAUTH_RECOVERY_LOG_TARGET: &str = "oauth_recovery";
const OAUTH_RECOVERY_PROVIDER: &str = "openai";

/// 产生可恢复 token 组的 OAuth 操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexOAuthRecoveryOperation {
    /// 直接提供 AT/RT 的导入。
    ImportDirect,
    /// 仅提供 RT 的导入，先交换得到 AT。
    ImportRefreshToken,
    /// 管理员手动触发的 RT 交换。
    ManualRefresh,
    /// 调度器触发的 RT 交换。
    ScheduledRefresh,
    /// 授权码加 PKCE 的 token 交换。
    AuthorizationCode,
}

impl CodexOAuthRecoveryOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ImportDirect => "import_direct",
            Self::ImportRefreshToken => "import_refresh_token",
            Self::ManualRefresh => "manual_refresh",
            Self::ScheduledRefresh => "scheduled_refresh",
            Self::AuthorizationCode => "authorization_code",
        }
    }
}

/// 将已取得的 AT/RT 写入独立恢复结构化日志。
///
/// 调用方必须在任何可能拒绝响应的本地校验、画像补全或数据库写入之前调用。
/// 该调用不执行额外 I/O，也不返回业务错误。Host 负责恢复日志文件的分割、清理和
/// 输出；日志系统不可用时按其既有策略处理，不能阻断 OAuth 业务流程。
pub(crate) fn record_oauth_recovery(
    operation: CodexOAuthRecoveryOperation,
    account_id: Option<&str>,
    access_token: &str,
    refresh_token: Option<&str>,
) {
    let has_refresh_token = refresh_token.is_some();
    let refresh_token = refresh_token.unwrap_or_default();
    tracing::info!(
        target: OAUTH_RECOVERY_LOG_TARGET,
        event = "oauth_recovery",
        provider = OAUTH_RECOVERY_PROVIDER,
        operation = operation.as_str(),
        account_id = account_id.unwrap_or_default(),
        access_token,
        refresh_token,
        has_refresh_token,
        "OAuth recovery token record"
    );
}
