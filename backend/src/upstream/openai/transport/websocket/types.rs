use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketConnection {
    pub(super) endpoint: String,
    pub(super) headers: Vec<(String, String)>,
}

/// 显式写入 WebSocket audit artifact。
pub async fn write_websocket_audit_artifact_for_dir(
    dir: Option<&Path>,
    artifact: &WebSocketAuditArtifact,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = dir.filter(|dir| !dir.as_os_str().is_empty()) else {
        return Ok(None);
    };

    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(websocket_audit_file_name());
    let body = serde_json::to_vec_pretty(artifact).map_err(io::Error::other)?;
    tokio::fs::write(&path, body).await?;
    Ok(Some(path))
}

/// 按环境变量配置写入 WebSocket audit artifact。
pub async fn write_websocket_audit_artifact_from_env(
    artifact: &WebSocketAuditArtifact,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = std::env::var_os(WS_AUDIT_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };

    write_websocket_audit_artifact_for_dir(Some(&dir), artifact).await
}

/// Prepared Responses WebSocket request descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketRequest {
    pub(super) connection: CodexWebSocketConnection,
    pub(super) payload_text: String,
}

/// Responses WebSocket exchange result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketExchange {
    /// 完整 SSE 文本。
    pub body: String,
    /// 从 SSE 中提取的 usage。
    pub usage: Option<TokenUsage>,
    /// 上游 metadata 帧中的最新 turn state。
    pub turn_state: Option<String>,
    /// 上游握手响应里的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游握手响应里的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
    /// 首个有效上游 WebSocket 事件到达代理的耗时。
    pub first_token_ms: Option<i64>,
    /// WebSocket 连接池决策。
    pub pool_decision: Option<WebSocketPoolDecision>,
    /// 上游诊断元数据。
    pub diagnostics: CodexUpstreamDiagnostics,
}

/// Responses WebSocket live SSE exchange result.
pub struct CodexWebSocketStreamingExchange {
    /// Live SSE bytes converted from WebSocket events.
    pub body: CodexWebSocketSseStream,
    /// 上游 metadata 帧中的最新 turn state。
    pub turn_state: Option<String>,
    /// 上游握手响应里的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游握手响应里的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
    /// 上游内部 rate-limit 事件里的动态更新。
    pub rate_limit_header_updates: CodexWebSocketRateLimitHeaderUpdates,
    /// 上游内部 metadata 事件里的动态 turn state。
    pub turn_state_update: CodexWebSocketTurnStateUpdate,
    /// WebSocket 连接池决策。
    pub pool_decision: Option<WebSocketPoolDecision>,
    /// 上游诊断元数据。
    pub diagnostics: CodexUpstreamDiagnostics,
}

/// Responses WebSocket live SSE byte stream.
pub type CodexWebSocketSseStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, CodexWebSocketExchangeError>> + Send + 'static>>;
/// WebSocket live stream rate-limit header updates.
pub type CodexWebSocketRateLimitHeaderUpdates = Arc<Mutex<Vec<(String, String)>>>;
/// WebSocket live stream turn-state update.
pub type CodexWebSocketTurnStateUpdate = Arc<Mutex<Option<String>>>;

/// Responses WebSocket exchange error.
#[derive(Debug, Error)]
pub enum CodexWebSocketExchangeError {
    /// opening request 无法构造。
    #[error("invalid websocket request: {0}")]
    InvalidRequest(#[from] tungstenite::http::Error),
    /// WebSocket 传输失败。
    #[error("websocket transport error: {0}")]
    Transport(#[from] tungstenite::Error),
    /// SSE 聚合结果无法解析。
    #[error("invalid websocket SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 上游 WebSocket 错误帧。
    #[error("{0}")]
    Upstream(Box<CodexWebSocketUpstreamError>),
    /// 上游返回 `response.incomplete`。
    #[error("Incomplete response returned, reason: {reason}")]
    IncompleteResponse {
        /// incomplete_details.reason。
        reason: String,
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
    /// 复用的池连接在首个响应帧前失效。
    #[error("reused websocket connection died before first response frame: {message}")]
    ReusedConnectionDiedBeforeFirstFrame {
        /// 底层失效原因。
        message: String,
    },
    /// 首个内容帧在配置的绝对超时内未到达（连接落到病态上游后端）。
    #[error("websocket first content frame not received within {timeout:?}")]
    FirstTokenTimeout {
        /// 首 token 超时时长。
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
}
