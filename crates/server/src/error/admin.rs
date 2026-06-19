//! 管理端错误映射。

/// 将领域错误格式化为 HTTP 错误消息。
pub fn format_admin_error(message: &str) -> String {
    message.to_string()
}
