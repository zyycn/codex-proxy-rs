//! Responses live stream 的规范事件与终态。

use serde_json::Value;

use crate::{
    dispatch::transport::canonical::{CanonicalResponseChunk, CanonicalStreamTerminal},
    upstream::openai::protocol::{events::TokenUsage, responses::ResponsesSseFailure},
};

/// live stream 离开事件循环的唯一终态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dispatch) enum StreamTerminal {
    Completed { response: Value },
    Incomplete { response: Value },
    Failed { failure: ResponsesSseFailure },
    UpstreamClosed,
    UpstreamError { detail: String },
    ProtocolError { detail: String },
    CaptureLimitExceeded,
    Cancelled,
    DownstreamClosed,
    Shutdown,
}

/// live loop 交给唯一 finalizer 的完整事实快照。
pub(in crate::dispatch) struct StreamSummary {
    pub terminal: StreamTerminal,
    pub body: Vec<u8>,
    pub terminal_chunks: Vec<CanonicalResponseChunk>,
    pub first_token_ms: Option<i64>,
    pub first_event_ms: i64,
    pub usage: Option<TokenUsage>,
    pub last_response_id: Option<String>,
}

impl From<CanonicalStreamTerminal> for StreamTerminal {
    fn from(terminal: CanonicalStreamTerminal) -> Self {
        match terminal {
            CanonicalStreamTerminal::Completed(response) => Self::Completed { response },
            CanonicalStreamTerminal::Incomplete(response) => Self::Incomplete { response },
            CanonicalStreamTerminal::Failed(failure) => Self::Failed { failure },
        }
    }
}

impl StreamTerminal {
    /// 只有上游没有给出业务终态时，代理才需要合成 `response.failed`。
    pub(in crate::dispatch) fn synthetic_failure_detail(&self) -> Option<&str> {
        match self {
            Self::UpstreamClosed => Some(""),
            Self::UpstreamError { detail } | Self::ProtocolError { detail } => Some(detail),
            Self::CaptureLimitExceeded => {
                Some("upstream response exceeded the 16 MiB proxy capture limit")
            }
            Self::Completed { .. }
            | Self::Incomplete { .. }
            | Self::Failed { .. }
            | Self::Cancelled
            | Self::DownstreamClosed
            | Self::Shutdown => None,
        }
    }

    pub(in crate::dispatch) fn should_finish_client_stream(&self) -> bool {
        !matches!(
            self,
            Self::Cancelled | Self::DownstreamClosed | Self::Shutdown
        )
    }

    pub(in crate::dispatch) fn name(&self) -> &'static str {
        match self {
            Self::Completed { .. } => "completed",
            Self::Incomplete { .. } => "incomplete",
            Self::Failed { .. } => "failed",
            Self::UpstreamClosed => "upstream_closed",
            Self::UpstreamError { .. } => "upstream_error",
            Self::ProtocolError { .. } => "protocol_error",
            Self::CaptureLimitExceeded => "capture_limit_exceeded",
            Self::Cancelled => "cancelled",
            Self::DownstreamClosed => "downstream_closed",
            Self::Shutdown => "shutdown",
        }
    }
}
