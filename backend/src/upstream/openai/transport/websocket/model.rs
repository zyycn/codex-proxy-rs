//! Responses WebSocket 请求与续接模型。

use std::fmt;

use crate::upstream::openai::protocol::responses::{
    CodexResponsesRequest, TransportRequirement, transport_requirement,
};

/// WebSocket opening 描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketConnection {
    pub(super) endpoint: String,
    pub(super) headers: Vec<(String, String)>,
}

/// Prepared Responses WebSocket request descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketRequest {
    pub(super) connection: CodexWebSocketConnection,
    pub(super) payload_text: String,
    pub(super) continuation: WebSocketContinuationRequirement,
}

/// 当前 WebSocket 请求对 previous response 状态的要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketContinuationRequirement {
    /// 不依赖任何已有响应状态。
    NewChain,
    /// 上游已持久化 response，可在新连接 hydration。
    Persisted { response_id: String },
    /// 代理没有所有权信息，只允许 dispatch 选定的单个账号原样尝试。
    ExternalUnknown { response_id: String },
    /// `store=false` response，只能在拥有该 ID 的原连接续接。
    ConnectionLocal { response_id: String },
}

impl WebSocketContinuationRequirement {
    pub(super) fn from_request(request: &CodexResponsesRequest) -> Self {
        let response_id = request.previous_response_id().map(ToString::to_string);
        match (transport_requirement(request), response_id) {
            (TransportRequirement::ExactWebSocketContinuation, Some(response_id)) => {
                Self::ConnectionLocal { response_id }
            }
            (TransportRequirement::PersistedContinuation, Some(response_id)) => {
                Self::Persisted { response_id }
            }
            (TransportRequirement::ExternalUnknown, Some(response_id)) => {
                Self::ExternalUnknown { response_id }
            }
            _ => Self::NewChain,
        }
    }

    pub(super) fn permits_fresh_connection(&self) -> bool {
        matches!(
            self,
            Self::NewChain | Self::Persisted { .. } | Self::ExternalUnknown { .. }
        )
    }
}

/// 连接本地 previous response 无法满足的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviousResponseUnavailableReason {
    PoolUnavailable,
    FreshConnectionRequired,
    ConnectionBusy,
    LatestResponseMismatch,
    ReusedConnectionLost,
    UpstreamRejected,
}

impl fmt::Display for PreviousResponseUnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PoolUnavailable => "pool_unavailable",
            Self::FreshConnectionRequired => "fresh_connection_required",
            Self::ConnectionBusy => "connection_busy",
            Self::LatestResponseMismatch => "latest_response_mismatch",
            Self::ReusedConnectionLost => "reused_connection_lost",
            Self::UpstreamRejected => "upstream_rejected",
        })
    }
}

impl CodexWebSocketRequest {
    /// 返回连接描述。
    pub fn connection(&self) -> &CodexWebSocketConnection {
        &self.connection
    }

    /// 返回将要发送的首个文本帧。
    pub fn payload_text(&self) -> &str {
        &self.payload_text
    }

    /// 返回连接续接要求。
    pub fn continuation(&self) -> &WebSocketContinuationRequirement {
        &self.continuation
    }
}

impl CodexWebSocketConnection {
    /// 构造待打开的 WebSocket 连接描述。
    pub fn new(endpoint: impl Into<String>, headers: Vec<(String, String)>) -> Self {
        Self {
            endpoint: endpoint.into(),
            headers,
        }
    }

    /// 返回 WebSocket endpoint。
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 返回按发送顺序保存的请求头。
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}
