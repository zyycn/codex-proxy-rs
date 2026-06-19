//! OpenAI 错误映射。

/// 将领域错误格式化为 OpenAI 错误消息。
pub fn format_openai_error(message: &str) -> String {
    message.to_string()
}
