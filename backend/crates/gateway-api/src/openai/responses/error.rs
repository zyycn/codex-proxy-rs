//! OpenAI Responses adapter 的稳定错误 contract。

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// OpenAI 风格的协议错误 JSON。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtocolErrorBody {
    /// 错误对象。
    pub error: ProtocolError,
}

/// 不包含请求正文或 prompt 的错误对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtocolError {
    /// OpenAI 错误类别。
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// 稳定机器码。
    pub code: &'static str,
    /// 安全的可读信息。
    pub message: String,
    /// 出错字段；不包含字段值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

impl ProtocolErrorBody {
    /// 序列化为 JSON value。
    #[must_use]
    pub fn into_value(self) -> Value {
        serde_json::json!({ "error": self.error })
    }
}

/// Responses 请求解码错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestDecodeError {
    /// 请求不是合法 JSON。
    #[error("request body must be valid JSON")]
    MalformedJson,
    /// 顶层不是 object。
    #[error("request body must be a JSON object")]
    ExpectedObject,
    /// 缺少必填字段。
    #[error("required field `{field}` is missing")]
    MissingField {
        /// 字段路径。
        field: String,
    },
    /// 字段类型错误。
    #[error("field `{field}` has an invalid type; expected {expected}")]
    InvalidType {
        /// 字段路径。
        field: String,
        /// 安全的期望类型。
        expected: &'static str,
    },
    /// 字段为空。
    #[error("field `{field}` must not be empty")]
    EmptyField {
        /// 字段路径。
        field: String,
    },
    /// 字段值不满足稳定约束。
    #[error("field `{field}` has an invalid value")]
    InvalidValue {
        /// 字段路径；不保存原始值。
        field: String,
    },
    /// 未知字段。
    #[error("unknown field `{field}`")]
    UnknownField {
        /// 字段路径；不保存字段值。
        field: String,
    },
    /// 已知但尚未实现的语义。
    #[error("field `{field}` is not supported by this gateway")]
    UnsupportedField {
        /// 字段路径。
        field: String,
    },
    /// Core operation 拒绝规范化后的字段。
    #[error("field `{field}` violates the canonical operation contract")]
    CanonicalContract {
        /// 字段路径。
        field: String,
    },
}

impl RequestDecodeError {
    /// 转换为 OpenAI 风格且不泄露正文的错误 payload。
    #[must_use]
    pub fn protocol_body(&self) -> ProtocolErrorBody {
        let (code, message, param) = match self {
            Self::MalformedJson => (
                "invalid_json",
                "Request body must be valid JSON.".to_owned(),
                None,
            ),
            Self::ExpectedObject => (
                "invalid_request_body",
                "Request body must be a JSON object.".to_owned(),
                None,
            ),
            Self::MissingField { field } => (
                "missing_required_parameter",
                format!("Required parameter `{field}` is missing."),
                Some(field.clone()),
            ),
            Self::InvalidType { field, expected } => (
                "invalid_type",
                format!("Parameter `{field}` must be {expected}."),
                Some(field.clone()),
            ),
            Self::EmptyField { field } => (
                "invalid_value",
                format!("Parameter `{field}` must not be empty."),
                Some(field.clone()),
            ),
            Self::InvalidValue { field } | Self::CanonicalContract { field } => (
                "invalid_value",
                format!("Parameter `{field}` has an invalid value."),
                Some(field.clone()),
            ),
            Self::UnknownField { field } => (
                "unknown_parameter",
                format!("Unknown parameter `{field}`."),
                Some(field.clone()),
            ),
            Self::UnsupportedField { field } => (
                "unsupported_parameter",
                format!("Parameter `{field}` is not supported by this gateway."),
                Some(field.clone()),
            ),
        };
        ProtocolErrorBody {
            error: ProtocolError {
                kind: "invalid_request_error",
                code,
                message,
                param,
            },
        }
    }
}

/// OpenAI Responses 原生 wire 转发错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResponseEncodeError {
    /// 协议原生流缺少可返回的终态 response object。
    #[error("responses wire stream has no terminal response")]
    MissingWireTerminal,
    /// 终态 response object 无法序列化为完整 HTTP JSON 响应。
    #[error("responses wire terminal serialization failed")]
    Serialization,
}

impl ResponseEncodeError {
    /// 转换为不包含生成内容的 OpenAI 风格错误 payload。
    #[must_use]
    pub fn protocol_body(&self) -> ProtocolErrorBody {
        ProtocolErrorBody {
            error: ProtocolError {
                kind: "server_error",
                code: "invalid_upstream_response",
                message: "The gateway could not forward the upstream response.".to_owned(),
                param: None,
            },
        }
    }
}
