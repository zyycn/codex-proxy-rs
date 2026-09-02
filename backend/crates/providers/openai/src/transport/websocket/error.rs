//! Responses WebSocket 分阶段错误。

use std::{fmt, time::Duration};

use gateway_protocol::openai::sse::SseError;
use thiserror::Error;
use uuid::Uuid;

use crate::transport::client::CodexClientVisibleUpstreamResponse;
use crate::transport::diagnostics::CodexUpstreamDiagnostics;
use crate::transport::diagnostics::CodexUpstreamSendPhase;

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
    /// 上游要求结束当前 WebSocket 连接并在新连接上重试。
    #[error("websocket connection limit reached")]
    ConnectionLimitReached,
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
    /// 建连并发送后，上游在配置时间内没有产生任何事件。
    #[error("websocket first upstream event not received within {timeout:?}")]
    InitialEventTimeout {
        /// 首个上游事件超时时长。
        timeout: Duration,
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

    /// opening 阶段只有明确的 transport 可用性失败才能切到同账号 HTTP。
    pub(in crate::transport) fn allows_pre_send_http_fallback(&self) -> bool {
        if let Self::ConnectionObserved { source, .. } = self {
            return source.allows_pre_send_http_fallback();
        }
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

    /// 精确连接状态不可用属于本地池路由事实，不消耗上游 WS 重试预算。
    pub(in crate::transport) fn requires_immediate_pool_fallback(&self) -> bool {
        matches!(self.classified(), Self::ContinuationUnavailable { .. })
    }

    /// 首个业务事件交付前，普通 WebSocket 失败可切到同账号 HTTP。
    pub(in crate::transport) fn allows_pre_delivery_http_fallback(&self) -> bool {
        match self {
            Self::Upstream(_) => false,
            Self::PostSendAmbiguous {
                source: Some(source),
                ..
            } => source.allows_pre_delivery_http_fallback(),
            Self::ConnectionObserved { source, .. } => source.allows_pre_delivery_http_fallback(),
            _ => true,
        }
    }
}
