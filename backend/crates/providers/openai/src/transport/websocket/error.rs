//! Responses WebSocket 分阶段错误。

use std::{fmt, time::Duration};

use gateway_protocol::openai::sse::SseError;
use thiserror::Error;

use crate::transport::diagnostics::CodexUpstreamDiagnostics;

use super::PreviousResponseUnavailableReason;

/// Responses WebSocket exchange error.
#[derive(Debug, Error)]
pub enum CodexWebSocketExchangeError {
    /// opening request 无法构造。
    #[error("invalid websocket request: {0}")]
    InvalidRequest(#[from] tungstenite::http::Error),
    /// WebSocket 传输失败。
    #[error("websocket transport error: {0}")]
    Transport(#[from] tungstenite::Error),
    /// DNS、TCP、TLS 或 opening handshake 在发送 payload 前失败。
    #[error("websocket connect failed before payload send: {0}")]
    Connect(#[source] tungstenite::Error),
    /// DNS、TCP、TLS 或 WebSocket upgrade 未在限定时间内完成。
    #[error("websocket connect timed out after {timeout:?}")]
    ConnectTimeout {
        /// 建连超时时长。
        timeout: Duration,
    },
    /// 普通请求的 WebSocket 快路径预算耗尽，payload 尚未发送。
    #[error("websocket fast-path connect budget exhausted after {timeout:?}")]
    FastPathTimeout {
        /// 快路径等待时长。
        timeout: Duration,
    },
    /// origin WebSocket 冷建连熔断中。
    #[error("websocket origin circuit is open")]
    OriginCircuitOpen,
    /// origin WebSocket 熔断器正在执行唯一 half-open 探针。
    #[error("websocket origin circuit half-open probe is already running")]
    OriginHalfOpenBusy,
    /// 同一精确会话的单飞建连失败。
    #[error("shared websocket connection attempt failed before payload send")]
    SharedConnectFailed,
    /// payload 已可能送达上游，禁止自动重放到其他 transport 或账号。
    #[error("websocket failed after payload send; replay outcome is ambiguous: {message}")]
    PostSendAmbiguous {
        /// 原始失败说明。
        message: String,
        /// 原始 typed transport/protocol failure。
        #[source]
        source: Option<Box<CodexWebSocketExchangeError>>,
    },
    /// 请求帧未在限定时间内写入上游连接。
    #[error("websocket request send timed out after {timeout:?}")]
    SendTimeout {
        /// 发送超时时长。
        timeout: Duration,
    },
    /// SSE 聚合结果无法解析。
    #[error("invalid websocket SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 上游 WebSocket 错误帧。
    #[error("{0}")]
    Upstream(Box<CodexWebSocketUpstreamError>),
    /// 请求依赖的连接本地 previous response 无法在当前连接满足。
    #[error("websocket continuation unavailable: {reason}")]
    ContinuationUnavailable {
        reason: PreviousResponseUnavailableReason,
    },
    /// 上游返回无法按官方形状解析的 `response.completed`。
    #[error("{message}")]
    InvalidCompletedResponse {
        /// 解析失败说明。
        message: String,
    },
    /// 上游在 terminal 事件前关闭。
    #[error("websocket closed before terminal event")]
    ClosedBeforeTerminal,
    /// 上游在指定时间内没有发送任何事件。
    #[error("websocket receive idle timeout after {timeout:?}")]
    ReceiveIdleTimeout {
        /// 超时时长。
        timeout: Duration,
    },
    /// 上游返回非文本事件帧。
    #[error("unexpected binary websocket event")]
    UnexpectedBinaryEvent,
    /// 复用的池连接在收到首个上游事件前失效。
    #[error("reused websocket connection died before first upstream event: {message}")]
    ReusedConnectionDiedBeforeFirstEvent {
        /// 底层失效原因。
        message: String,
    },
    /// 建连并发送后，上游在配置时间内没有产生任何事件。
    #[error("websocket first upstream event not received within {timeout:?}")]
    InitialEventTimeout {
        /// 首个上游事件超时时长。
        timeout: Duration,
    },
}

/// WebSocket 上游错误帧载荷。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketUpstreamError {
    /// HTTP-style upstream status code.
    pub status_code: u16,
    /// 推导出的重试秒数。
    pub retry_after_seconds: Option<u64>,
    /// 原始错误帧。
    pub body: String,
    /// 上游透传的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游诊断元数据。
    pub diagnostics: CodexUpstreamDiagnostics,
}

impl fmt::Display for CodexWebSocketUpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "websocket upstream error {}: {}",
            self.status_code, self.body
        )
    }
}

impl CodexWebSocketExchangeError {
    pub(super) fn upstream(
        status_code: u16,
        retry_after_seconds: Option<u64>,
        body: String,
        set_cookie_headers: Vec<String>,
        diagnostics: CodexUpstreamDiagnostics,
    ) -> Self {
        Self::Upstream(Box::new(CodexWebSocketUpstreamError {
            status_code,
            retry_after_seconds,
            body,
            set_cookie_headers,
            diagnostics,
        }))
    }

    /// opening 阶段只有明确的 transport 可用性失败才能切到同账号 HTTP。
    pub(in crate::transport) fn allows_pre_send_http_fallback(&self) -> bool {
        matches!(
            self,
            Self::Connect(_)
                | Self::ConnectTimeout { .. }
                | Self::FastPathTimeout { .. }
                | Self::OriginCircuitOpen
                | Self::OriginHalfOpenBusy
                | Self::SharedConnectFailed
                | Self::ContinuationUnavailable { .. }
        )
    }
}
