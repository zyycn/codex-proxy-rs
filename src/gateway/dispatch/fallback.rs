//! 上游回退策略。

/// 判断状态码是否表示上游限流。
pub fn status_code_is_rate_limited(status_code: u16) -> bool {
    status_code == 429
}

/// 判断状态码是否表示账号配额耗尽。
pub fn status_code_is_quota_exhausted(status_code: u16) -> bool {
    status_code == 402
}

/// 判断状态码是否表示可换账号重试的临时上游错误。
pub fn status_code_is_transient_upstream(status_code: u16) -> bool {
    matches!(status_code, 500..=599)
}
