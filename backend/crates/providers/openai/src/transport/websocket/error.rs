//! Responses WebSocket 分阶段错误。

use std::{fmt, time::Duration};

use gateway_protocol::openai::sse::SseError;
use thiserror::Error;
use uuid::Uuid;

use crate::transport::client::CodexClientVisibleUpstreamResponse;
use crate::transport::diagnostics::CodexUpstreamDiagnostics;
use crate::transport::diagnostics::CodexUpstreamSendPhase;
use crate::transport::protocol::responses::ResponsesSseFailure;

use super::PreviousResponseUnavailableReason;
use super::pump::WebSocketConnectionObservation;

/// Responses WebSocket 交互错误。
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
    /// 上游要求结束当前 WebSocket 连接并在新连接上重试。
    #[error("websocket connection limit reached")]
    ConnectionLimitReached(Box<ResponsesSseFailure>),
    /// 请求依赖的连接本地 previous response 无法在当前连接满足。
    #[error("websocket continuation unavailable: {reason}")]
    ContinuationUnavailable {
        reason: PreviousResponseUnavailableReason,
    },
    /// 上游在 terminal 事件前关闭。
    #[error("{0}")]
    ClosedBeforeTerminal(CodexWebSocketCloseError),
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
        /// 原始 typed transport failure。
        #[source]
        source: Option<Box<CodexWebSocketExchangeError>>,
    },
    /// 将一个已分类交互错误与物理连接生命周期快照绑定。
    #[error("{source}")]
    ConnectionObserved {
        observation: WebSocketConnectionObservation,
        #[source]
        source: Box<CodexWebSocketExchangeError>,
    },
}

/// 上游在 terminal 事件前发送的 WebSocket close 信息。
///
/// close reason 只供原请求的客户端错误响应使用；`Debug` 与 `Display` 均不会输出它。
#[derive(Clone, PartialEq, Eq)]
pub struct CodexWebSocketCloseError {
    connection_id: Option<Uuid>,
    code: Option<u16>,
    reason: Option<String>,
    last_event_type: Option<String>,
}

impl CodexWebSocketCloseError {
    pub(crate) fn new(code: Option<u16>, reason: Option<String>) -> Self {
        Self {
            connection_id: None,
            code,
            reason,
            last_event_type: None,
        }
    }

    pub(crate) fn with_connection_id(mut self, connection_id: Uuid) -> Self {
        self.connection_id = Some(connection_id);
        self
    }

    pub(crate) fn with_last_event_type(mut self, event_type: Option<String>) -> Self {
        self.last_event_type = event_type;
        self
    }

    /// 返回承载该 close frame 的 WebSocket 连接标识。
    #[must_use]
    pub const fn connection_id(&self) -> Option<Uuid> {
        self.connection_id
    }

    /// 返回上游 close code；没有 close frame 时为 `None`。
    #[must_use]
    pub const fn code(&self) -> Option<u16> {
        self.code
    }

    /// 返回上游 close reason；仅可用于当前请求的客户端协议响应。
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// 返回关闭前最后一条经过白名单校验的事件类型；不包含事件 payload。
    #[must_use]
    pub fn last_event_type(&self) -> Option<&str> {
        self.last_event_type.as_deref()
    }
}

impl fmt::Debug for CodexWebSocketCloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexWebSocketCloseError")
            .field("connection_id", &self.connection_id)
            .field("code", &self.code)
            .field("reason", &self.reason.as_ref().map(|_| "<redacted>"))
            .field("last_event_type", &self.last_event_type)
            .finish()
    }
}

impl fmt::Display for CodexWebSocketCloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(
                formatter,
                "websocket closed before terminal event (code {code})"
            ),
            None => formatter.write_str("websocket closed before terminal event"),
        }
    }
}

/// WebSocket 上游错误帧载荷。
#[derive(Clone, PartialEq, Eq)]
pub struct CodexWebSocketUpstreamError {
    /// 上游返回的 HTTP 风格状态码。
    pub status_code: u16,
    /// 推导出的重试秒数。
    pub retry_after_seconds: Option<u64>,
    /// 原始错误帧。
    pub body: String,
    /// opening 失败时可返回给当前客户端的原始 HTTP 响应。
    pub client_response: Option<Box<CodexClientVisibleUpstreamResponse>>,
    /// 上游透传的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游诊断元数据。
    pub diagnostics: CodexUpstreamDiagnostics,
    /// 上游拒绝相对业务 payload 的发送阶段。
    pub send_phase: CodexUpstreamSendPhase,
}

impl fmt::Debug for CodexWebSocketUpstreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexWebSocketUpstreamError")
            .field("status_code", &self.status_code)
            .field("retry_after_seconds", &self.retry_after_seconds)
            .field("body", &"<redacted>")
            .field(
                "client_response",
                &self.client_response.as_ref().map(|_| "<present>"),
            )
            .field("set_cookie_headers", &self.set_cookie_headers.len())
            .field("diagnostics", &self.diagnostics)
            .field("send_phase", &self.send_phase)
            .finish()
    }
}

impl fmt::Display for CodexWebSocketUpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "websocket upstream returned status {}", self.status_code)
    }
}

impl CodexWebSocketExchangeError {
    /// 仅供诊断展示穿透发送状态与连接快照包装；重试分类仍使用 classified。
    pub(crate) fn diagnostic_cause(&self) -> &Self {
        match self {
            Self::PostSendAmbiguous {
                source: Some(source),
                ..
            }
            | Self::ReusedConnectionDiedBeforeFirstEvent {
                source: Some(source),
                ..
            }
            | Self::ConnectionObserved { source, .. } => source.diagnostic_cause(),
            _ => self,
        }
    }

    /// 返回可持久化的传输错误分类，不包含底层错误中的地址、报文或凭据。
    /// 与已有指标粗分类分开：缺少关闭握手本身不能证明收到 TCP RST。
    pub(crate) fn transport_failure_reason(&self) -> Option<&'static str> {
        match self.diagnostic_cause() {
            Self::Transport(error) | Self::Connect(error) => Some(match error {
                tungstenite::Error::Protocol(
                    tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                ) => "reset_without_closing_handshake",
                tungstenite::Error::Io(error) => match error.kind() {
                    std::io::ErrorKind::ConnectionRefused => "connection_refused",
                    std::io::ErrorKind::NetworkUnreachable => "network_unreachable",
                    std::io::ErrorKind::HostUnreachable => "host_unreachable",
                    std::io::ErrorKind::ConnectionReset => "tcp_reset",
                    std::io::ErrorKind::ConnectionAborted => "connection_aborted",
                    std::io::ErrorKind::BrokenPipe => "broken_pipe",
                    std::io::ErrorKind::UnexpectedEof => "unexpected_eof",
                    std::io::ErrorKind::TimedOut => "transport_timeout",
                    _ => "io_error",
                },
                tungstenite::Error::Tls(_) => "tls_error",
                tungstenite::Error::Protocol(_) => "protocol_error",
                tungstenite::Error::Capacity(_) => "capacity_error",
                _ => "transport_error",
            }),
            _ => None,
        }
    }

    pub(crate) fn closed_before_terminal_on(
        connection_id: Uuid,
        code: Option<u16>,
        reason: Option<String>,
        last_event_type: Option<String>,
    ) -> Self {
        Self::ClosedBeforeTerminal(
            CodexWebSocketCloseError::new(code, reason)
                .with_connection_id(connection_id)
                .with_last_event_type(last_event_type),
        )
    }

    /// 返回错误链中的上游终态前 Close 帧（若存在）。
    #[must_use]
    pub fn close_before_terminal(&self) -> Option<&CodexWebSocketCloseError> {
        match self {
            Self::ClosedBeforeTerminal(close) => Some(close),
            Self::PostSendAmbiguous {
                source: Some(source),
                ..
            }
            | Self::ReusedConnectionDiedBeforeFirstEvent {
                source: Some(source),
                ..
            }
            | Self::ConnectionObserved { source, .. } => source.close_before_terminal(),
            _ => None,
        }
    }

    pub(crate) fn with_connection_observation(
        self,
        observation: WebSocketConnectionObservation,
    ) -> Self {
        match self {
            Self::ConnectionObserved { .. } => self,
            source => Self::ConnectionObserved {
                observation,
                source: Box::new(source),
            },
        }
    }

    /// 返回错误关联的物理 WebSocket 连接生命周期快照。
    #[must_use]
    pub fn connection_observation(&self) -> Option<&WebSocketConnectionObservation> {
        match self {
            Self::ConnectionObserved { observation, .. } => Some(observation),
            Self::PostSendAmbiguous {
                source: Some(source),
                ..
            }
            | Self::ReusedConnectionDiedBeforeFirstEvent {
                source: Some(source),
                ..
            } => source.connection_observation(),
            _ => None,
        }
    }

    /// 返回去掉观测 wrapper 后的原始分类错误。
    pub(crate) fn classified(&self) -> &Self {
        match self {
            Self::ConnectionObserved { source, .. } => source.classified(),
            _ => self,
        }
    }

    #[must_use]
    pub fn continuation_unavailable_reason(&self) -> Option<PreviousResponseUnavailableReason> {
        match self.classified() {
            Self::ContinuationUnavailable { reason } => Some(*reason),
            _ => None,
        }
    }

    pub(super) fn upstream(
        status_code: u16,
        retry_after_seconds: Option<u64>,
        body: String,
        client_response: Option<Box<CodexClientVisibleUpstreamResponse>>,
        set_cookie_headers: Vec<String>,
        diagnostics: CodexUpstreamDiagnostics,
        send_phase: CodexUpstreamSendPhase,
    ) -> Self {
        Self::Upstream(Box::new(CodexWebSocketUpstreamError {
            status_code,
            retry_after_seconds,
            body,
            client_response,
            set_cookie_headers,
            diagnostics,
            send_phase,
        }))
    }
}
