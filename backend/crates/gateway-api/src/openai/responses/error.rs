//! OpenAI Responses adapter 的稳定错误 contract。

use gateway_core::event::EventSequenceError;
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
    /// Provider options 版本不受支持。
    #[error("unsupported provider_options version")]
    UnsupportedProviderOptionsVersion,
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
            Self::UnsupportedProviderOptionsVersion => (
                "unsupported_provider_options_version",
                "The provider_options version is not supported.".to_owned(),
                Some("provider_options.version".to_owned()),
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

/// Canonical event 到 OpenAI Responses 的编码错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResponseEncodeError {
    /// Core canonical 顺序错误。
    #[error("canonical event sequence is invalid: {0}")]
    Sequence(#[from] EventSequenceError),
    /// Core 新增了当前 adapter 尚未声明的事件语义。
    #[error("canonical event is not supported by this OpenAI Responses adapter")]
    UnsupportedEvent,
    /// `Started` 与 `Completed` 元数据不一致。
    #[error("response metadata changed during canonical stream")]
    MetadataChanged,
    /// 同一 stream 重复报告 usage。
    #[error("canonical stream contains more than one Usage event")]
    DuplicateUsage,
    /// 当前 OpenAI Responses adapter 无法表达该 canonical 内容类别。
    #[error("canonical content kind `{kind}` is not supported by OpenAI Responses")]
    UnsupportedContentKind {
        /// 安全类别名。
        kind: &'static str,
    },
    /// Usage 缺少 OpenAI Responses 所需的基础 token 事实。
    #[error("canonical Usage is missing required token totals")]
    IncompleteUsage,
    /// Tool call 在结束时仍缺少名称。
    #[error("tool call at content index {index} has no name")]
    MissingToolName {
        /// Canonical content index。
        index: u32,
    },
    /// Tool call ID 或名称在增量中发生变化。
    #[error("tool call identity changed at content index {index}")]
    ToolIdentityChanged {
        /// Canonical content index。
        index: u32,
    },
    /// Collector 已终结。
    #[error("responses collector has already completed")]
    AlreadyCompleted,
    /// Adapter 自身生成的 event framing 无法还原为 WebSocket event JSON。
    #[error("responses event encoder produced an invalid internal framing")]
    InvalidEventEncoding,
    /// 同一响应混用了协议原生与 canonical 表达。
    #[error("responses stream changed its wire representation")]
    MixedWireRepresentation,
    /// 协议原生流缺少可返回的终态 response object。
    #[error("responses wire stream has no terminal response")]
    MissingWireTerminal,
    /// 协议原生事件携带了不一致的响应身份。
    #[error("responses wire stream changed its response identity")]
    WireIdentityChanged,
}

impl ResponseEncodeError {
    /// 转换为不包含生成内容的 OpenAI 风格错误 payload。
    #[must_use]
    pub fn protocol_body(&self) -> ProtocolErrorBody {
        ProtocolErrorBody {
            error: ProtocolError {
                kind: "server_error",
                code: "invalid_canonical_response",
                message: "The gateway could not encode the canonical response.".to_owned(),
                param: None,
            },
        }
    }
}
