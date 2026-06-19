//! OpenAI 错误响应类型。

use serde::{Deserialize, Serialize};

/// OpenAI 错误对象。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiError {
    /// 错误消息。
    pub message: String,
    /// 错误类型。
    #[serde(rename = "type")]
    pub error_type: String,
    /// 错误代码。
    pub code: Option<String>,
}
