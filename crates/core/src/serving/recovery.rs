//! 错误恢复规则。

/// 判断状态码是否允许在同一账号上重试。
pub fn status_code_allows_same_account_retry(status_code: u16) -> bool {
    matches!(status_code, 500..=599)
}
