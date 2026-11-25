//! OpenAI Responses adapter 的稳定错误 contract

use crate::client_error::{ProtocolError, ProtocolErrorBody};
use thiserror::Error;

/// OpenAI Responses 原生 wire 转发错误
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResponseEncodeError {
    /// 协议原生流缺少可返回的终态 response object
    #[error("responses wire stream has no terminal response")]
    MissingWireTerminal,
    /// 缺少完整终态输出，已交付的完成项也存在缺项或冲突，不能伪造完整结果
    #[error("responses wire stream has inconsistent output items")]
    InvalidOutputItems,
    /// 终态 response object 无法序列化为完整 HTTP JSON 响应
    #[error("responses wire terminal serialization failed")]
    Serialization,
}

impl ResponseEncodeError {
    /// 转换为不包含生成内容的 OpenAI 风格错误 payload
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
