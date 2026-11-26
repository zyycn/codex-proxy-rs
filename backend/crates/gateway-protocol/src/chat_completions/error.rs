//! Chat 解析与直接转换的安全错误事实

use thiserror::Error;

/// 协议层错误不持有请求内容或上游凭据
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChatError {
    #[error("Request body must be valid JSON.")]
    MalformedJson,
    #[error("Request body must be a JSON object.")]
    ExpectedObject,
    #[error("Required parameter `{field}` is missing.")]
    MissingField { field: String },
    #[error("Parameter `{field}` must be {expected}.")]
    InvalidType {
        field: String,
        expected: &'static str,
    },
    #[error("Parameter `{field}` must not be empty.")]
    EmptyField { field: String },
    #[error("Parameter `{field}` has an invalid value.")]
    InvalidValue { field: String },
    #[error("Unknown parameter `{field}`.")]
    UnknownField { field: String },
    #[error("Parameter `{field}` is not supported by this upstream.")]
    UnsupportedField { field: String },
    #[error("The requested `{field}` is not supported with `{other}`.")]
    UnsupportedCombination {
        field: &'static str,
        other: &'static str,
    },
    #[error("The upstream response cannot be represented as a Chat completion.")]
    InvalidResponse,
    #[error("The upstream stream ended before its terminal event.")]
    IncompleteStream,
}

impl ChatError {
    /// 供 API 出口构造稳定的机器码
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MalformedJson => "invalid_json",
            Self::ExpectedObject => "invalid_request_body",
            Self::MissingField { .. } => "missing_required_parameter",
            Self::InvalidType { .. } => "invalid_type",
            Self::EmptyField { .. } | Self::InvalidValue { .. } => "invalid_value",
            Self::UnknownField { .. } => "unknown_parameter",
            Self::UnsupportedField { .. } | Self::UnsupportedCombination { .. } => {
                "unsupported_parameter"
            }
            Self::InvalidResponse => "invalid_upstream_response",
            Self::IncompleteStream => "incomplete_upstream_response",
        }
    }

    /// 字段路径仅包含协议结构，不包含用户值
    pub fn parameter(&self) -> Option<&str> {
        match self {
            Self::MissingField { field }
            | Self::InvalidType { field, .. }
            | Self::EmptyField { field }
            | Self::InvalidValue { field }
            | Self::UnknownField { field }
            | Self::UnsupportedField { field } => Some(field),
            Self::UnsupportedCombination { field, .. } => Some(field),
            _ => None,
        }
    }
}

pub(super) fn invalid(field: &str) -> ChatError {
    ChatError::InvalidValue {
        field: field.to_owned(),
    }
}

pub(super) fn unsupported(field: &str) -> ChatError {
    ChatError::UnsupportedField {
        field: field.to_owned(),
    }
}
