//! 上游发送前的准备合同；正文由 RPC 二进制载荷承载

use std::fmt;

use serde::{Deserialize, Serialize};

use super::MiddlewareHeader;

/// HTTP 发送目标或 WebSocket 候选 opening，返回值必须保留传输类型
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Http {
        method: String,
        uri: String,
    },
    #[serde(rename = "websocket")]
    WebSocket {
        uri: String,
    },
}

impl fmt::Debug for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { method, .. } => formatter
                .debug_struct("Http")
                .field("method", method)
                .finish_non_exhaustive(),
            Self::WebSocket { .. } => formatter.debug_struct("WebSocket").finish_non_exhaustive(),
        }
    }
}

/// HTTP 正文是实际内容编码后的字节；WebSocket 头是候选 opening
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestHead {
    pub target: Target,
    pub headers: Vec<MiddlewareHeader>,
}

impl fmt::Debug for RequestHead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestHead")
            .field("target", &self.target)
            .field("headers", &self.headers.len())
            .finish()
    }
}

/// 宿主冻结的实际业务归属；改写请求不会改变账号、调用关联或计量归属
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub request_id: String,
    pub client_key_id: String,
    pub account_group_ids: Vec<String>,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub account_id: String,
    pub attempt_index: u32,
}

/// 入口请求与宿主归属；最终响应只返回 RequestHead 和正文载荷
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallHead {
    pub request: RequestHead,
    pub metadata: Metadata,
}
