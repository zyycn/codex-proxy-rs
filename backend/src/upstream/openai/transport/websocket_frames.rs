//! WebSocket 帧交换、流转发与错误类型。

use super::*;

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
    /// 安全响应元数据。
    pub response_metadata: CodexResponseMetadata,
}

/// Responses WebSocket live SSE exchange result.
pub struct CodexWebSocketStreamingExchange {
    /// 关联请求日志与底层 pump 生命周期日志的连接标识。
    pub(crate) websocket_connection_id: Uuid,
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
    /// 安全响应元数据。
    pub response_metadata: CodexResponseMetadata,
}

/// Responses WebSocket live SSE byte stream.
pub type CodexWebSocketSseStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, CodexWebSocketExchangeError>> + Send + 'static>>;
/// WebSocket live stream rate-limit header updates.
pub type CodexWebSocketRateLimitHeaderUpdates = Arc<Mutex<Vec<(String, String)>>>;
/// WebSocket live stream turn-state update.
pub type CodexWebSocketTurnStateUpdate = Arc<Mutex<Option<String>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WebSocketTerminalKind {
    Completed,
    Incomplete,
    Failed,
}

/// Responses WebSocket exchange error.
#[derive(Debug, Error)]
pub enum CodexWebSocketExchangeError {
    /// opening request 无法构造。
    #[error("invalid websocket request: {0}")]
    InvalidRequest(#[from] tungstenite::http::Error),
    /// WebSocket 传输失败。
    #[error("websocket transport error: {0}")]
    Transport(#[from] tungstenite::Error),
    /// DNS、TCP、TLS 或 WebSocket upgrade 未在限定时间内完成。
    #[error("websocket connect timed out after {timeout:?}")]
    ConnectTimeout {
        /// 建连超时时长。
        timeout: Duration,
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
    ReusedConnectionDiedBeforeFirstOutput {
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
    /// 上游错误类型。
    pub error_type: Option<String>,
    /// 上游错误码。
    pub code: Option<String>,
    /// 上游错误消息。
    pub message: Option<String>,
    /// 上游错误参数。
    pub param: Option<String>,
    /// 错误事件携带的响应头。
    pub headers: Vec<(String, String)>,
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
            error_type: None,
            code: None,
            message: None,
            param: None,
            headers: Vec::new(),
            set_cookie_headers,
            diagnostics,
        }))
    }
}

/// 执行一次 prepared Responses WebSocket 请求并聚合为 SSE。
pub async fn execute_response_create_request(
    request: &CodexWebSocketRequest,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    execute_response_create_request_with_pool(request, None, Instant::now(), None).await
}

// ---------------------------------------------------------------------------
// Response collection
// ---------------------------------------------------------------------------

pub(super) async fn collect_websocket_response(
    mut websocket: PumpedWebSocket,
    mut metadata: CodexWebSocketConnectionMetadata,
    mut continuation: WebSocketContinuationState,
    reused_connection: bool,
    started_at: Instant,
    initial_event_timeout: Option<Duration>,
) -> Result<
    (
        CodexWebSocketExchange,
        PumpedWebSocket,
        CodexWebSocketConnectionMetadata,
        WebSocketContinuationState,
        WebSocketTerminalKind,
    ),
    CodexWebSocketExchangeError,
> {
    let mut body = String::new();
    let mut saw_upstream_activity = false;
    let mut first_token_ms = None;

    loop {
        let receive_timeout = receive_idle_timeout(saw_upstream_activity, initial_event_timeout);
        let message = match next_websocket_message(&mut websocket, receive_timeout).await {
            Ok(message) => message,
            Err(CodexWebSocketExchangeError::ReceiveIdleTimeout { timeout })
                if !saw_upstream_activity =>
            {
                if reused_connection {
                    return Err(reused_connection_died_before_first_output(
                        &CodexWebSocketExchangeError::InitialEventTimeout { timeout },
                    ));
                }
                return Err(CodexWebSocketExchangeError::InitialEventTimeout { timeout });
            }
            Err(error) => return Err(error),
        };
        let Some(message) = message else {
            break;
        };
        let text = match message {
            Message::Text(text) => {
                saw_upstream_activity = true;
                text
            }
            Message::Binary(_) => return Err(CodexWebSocketExchangeError::UnexpectedBinaryEvent),
            Message::Close(_) if reused_connection && !saw_upstream_activity => {
                return Err(
                    CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                        message: "websocket closed".to_string(),
                    },
                );
            }
            Message::Close(_) => break,
            _ => continue,
        };
        let raw = text.to_string();
        if let Some(headers) = websocket_rate_limit_event_headers(&raw) {
            metadata.rate_limit_headers.extend(headers);
            continue;
        }
        update_websocket_response_metadata(&mut metadata, &raw);
        if let Some(metadata_turn_state) = websocket_metadata_turn_state(&raw) {
            metadata.turn_state = Some(metadata_turn_state);
            continue;
        }
        let event = websocket_event_type(&raw);
        if event.as_deref() == Some("response.completed") {
            let response_id = websocket_response_completed_id(&raw)
                .map_err(
                    |message| CodexWebSocketExchangeError::InvalidCompletedResponse { message },
                )?
                .ok_or_else(|| CodexWebSocketExchangeError::InvalidCompletedResponse {
                    message: "response.completed is missing response id".to_string(),
                })?;
            continuation.record_completed(response_id);
        }
        let terminal = match event.as_deref() {
            Some("response.completed") => Some(WebSocketTerminalKind::Completed),
            Some("response.incomplete") => Some(WebSocketTerminalKind::Incomplete),
            Some("response.failed" | "error") => Some(WebSocketTerminalKind::Failed),
            _ => None,
        };
        let forwarded = if let Some(frame) = websocket_event_to_sse_frame(&raw) {
            let has_first_output = response_body_has_first_output(frame.as_bytes());
            body.push_str(&frame);
            if has_first_output {
                response_meta::update_first_token_ms(
                    started_at,
                    body.as_bytes(),
                    &mut first_token_ms,
                );
            }
            true
        } else {
            false
        };
        if let (true, Some(terminal)) = (forwarded, terminal) {
            let usage = events::extract_sse_usage(&body)?;
            let exchange = CodexWebSocketExchange {
                body,
                usage,
                turn_state: metadata.turn_state.clone(),
                set_cookie_headers: metadata.set_cookie_headers.clone(),
                rate_limit_headers: metadata.rate_limit_headers.clone(),
                first_token_ms,
                pool_decision: None,
                diagnostics: metadata.diagnostics.clone(),
                response_metadata: metadata.response_metadata.clone(),
            };
            return Ok((exchange, websocket, metadata, continuation, terminal));
        }
    }

    if reused_connection && !saw_upstream_activity {
        return Err(
            CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                message: "websocket closed before terminal event".to_string(),
            },
        );
    }

    Err(CodexWebSocketExchangeError::ClosedBeforeTerminal)
}

fn reused_stream_receive_error(error: CodexWebSocketExchangeError) -> CodexWebSocketExchangeError {
    match error {
        CodexWebSocketExchangeError::ClosedBeforeTerminal
        | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
        | CodexWebSocketExchangeError::Transport(_) => {
            reused_connection_died_before_first_output(&error)
        }
        error => error,
    }
}

fn reused_connection_died_before_first_output(
    error: &CodexWebSocketExchangeError,
) -> CodexWebSocketExchangeError {
    CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
        message: error.to_string(),
    }
}

pub(super) fn websocket_connection_metadata(
    response: &WsResponse<Option<Vec<u8>>>,
) -> CodexWebSocketConnectionMetadata {
    CodexWebSocketConnectionMetadata {
        turn_state: response_meta::turn_state(response.headers()),
        set_cookie_headers: response_meta::set_cookie_headers(response.headers()),
        rate_limit_headers: response_meta::rate_limit_headers(response.headers()),
        response_metadata: response_meta::response_metadata(response.headers()),
        diagnostics: response_meta::diagnostics(
            Some(response.status().as_u16()),
            response.headers(),
        ),
    }
}

pub(super) fn reusable_websocket_metadata(
    mut metadata: CodexWebSocketConnectionMetadata,
) -> CodexWebSocketConnectionMetadata {
    metadata.rate_limit_headers.clear();
    metadata
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

pub(super) struct WebSocketStreamPoolReturn {
    pub(super) lease: WebSocketPoolLease,
    pub(super) created_at: tokio::time::Instant,
    pub(super) continuation: WebSocketContinuationState,
}

#[derive(Debug, Clone, Copy)]
enum StreamWebSocketDiscardReason {
    ClientCancelled,
    DownstreamSendFailed,
    IncompleteResponse,
    FailedResponse,
    InvalidCompletedResponse,
    UnexpectedBinaryEvent,
    UpstreamClosed,
    UpstreamReceiveFailed,
}

impl StreamWebSocketDiscardReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClientCancelled => "client_cancelled",
            Self::DownstreamSendFailed => "downstream_send_failed",
            Self::IncompleteResponse => "incomplete_response",
            Self::FailedResponse => "failed_response",
            Self::InvalidCompletedResponse => "invalid_completed_response",
            Self::UnexpectedBinaryEvent => "unexpected_binary_event",
            Self::UpstreamClosed => "upstream_closed",
            Self::UpstreamReceiveFailed => "upstream_receive_failed",
        }
    }
}

pub(super) fn stream_websocket_response(
    websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
    reused_connection: bool,
    initial_event_timeout: Option<Duration>,
) -> CodexWebSocketStreamingExchange {
    let websocket_connection_id = websocket.connection_id();
    let response_metadata = metadata.clone();
    let rate_limit_header_updates = Arc::new(Mutex::new(Vec::new()));
    let rate_limit_header_updates_for_task = Arc::clone(&rate_limit_header_updates);
    let turn_state_update = Arc::new(Mutex::new(metadata.turn_state.clone()));
    let turn_state_update_for_task = Arc::clone(&turn_state_update);
    let (tx, rx) = mpsc::channel(WEBSOCKET_STREAM_BUFFER);
    tokio::spawn(async move {
        forward_websocket_response_stream(WebSocketStreamForwardState {
            websocket,
            metadata,
            pool_return,
            reused_connection,
            initial_event_timeout,
            rate_limit_header_updates: rate_limit_header_updates_for_task,
            turn_state_update: turn_state_update_for_task,
            tx,
        })
        .await;
    });

    let body = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });

    CodexWebSocketStreamingExchange {
        websocket_connection_id,
        body: Box::pin(body),
        turn_state: response_metadata.turn_state,
        set_cookie_headers: response_metadata.set_cookie_headers,
        rate_limit_headers: response_metadata.rate_limit_headers,
        rate_limit_header_updates,
        turn_state_update,
        pool_decision: None,
        diagnostics: response_metadata.diagnostics,
        response_metadata: response_metadata.response_metadata,
    }
}

struct WebSocketStreamForwardState {
    websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
    reused_connection: bool,
    initial_event_timeout: Option<Duration>,
    rate_limit_header_updates: CodexWebSocketRateLimitHeaderUpdates,
    turn_state_update: CodexWebSocketTurnStateUpdate,
    tx: mpsc::Sender<Result<Bytes, CodexWebSocketExchangeError>>,
}

async fn forward_websocket_response_stream(state: WebSocketStreamForwardState) {
    let WebSocketStreamForwardState {
        mut websocket,
        mut metadata,
        pool_return,
        reused_connection,
        initial_event_timeout,
        rate_limit_header_updates,
        turn_state_update,
        tx,
    } = state;
    let mut pool_return = pool_return;
    let mut continuation = pool_return
        .as_mut()
        .map(|pool_return| std::mem::take(&mut pool_return.continuation))
        .unwrap_or_default();
    let mut saw_upstream_activity = false;
    loop {
        let message = tokio::select! {
            biased;
            // 客户端断开下游 SSE 流：立即丢弃连接并释放池 slot，
            // 不再傻等上游 idle 超时（否则同会话后续请求会一直 bypass/busy）。
            () = tx.closed() => {
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::ClientCancelled,
                ).await;
                return;
            }
            message = next_websocket_message(
                &mut websocket,
                receive_idle_timeout(saw_upstream_activity, initial_event_timeout),
            ) => message,
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::UpstreamReceiveFailed,
                )
                .await;
                let error = if reused_connection {
                    reused_stream_receive_error(error)
                } else {
                    error
                };
                let _ = tx.send(Err(error)).await;
                return;
            }
        };
        let Some(message) = message else {
            break;
        };
        let raw = match message {
            Message::Text(text) => {
                saw_upstream_activity = true;
                text.to_string()
            }
            Message::Binary(_) => {
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::UnexpectedBinaryEvent,
                )
                .await;
                let _ = tx
                    .send(Err(CodexWebSocketExchangeError::UnexpectedBinaryEvent))
                    .await;
                return;
            }
            Message::Close(_) => {
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::UpstreamClosed,
                )
                .await;
                let error = CodexWebSocketExchangeError::ClosedBeforeTerminal;
                let error = if reused_connection {
                    reused_stream_receive_error(error)
                } else {
                    error
                };
                let _ = tx.send(Err(error)).await;
                return;
            }
            _ => continue,
        };
        if let Some(headers) = websocket_rate_limit_event_headers(&raw) {
            metadata.rate_limit_headers.extend(headers.iter().cloned());
            rate_limit_header_updates.lock().await.extend(headers);
            continue;
        }
        update_websocket_response_metadata(&mut metadata, &raw);
        if let Some(metadata_turn_state) = websocket_metadata_turn_state(&raw) {
            metadata.turn_state = Some(metadata_turn_state);
            *turn_state_update.lock().await = metadata.turn_state.clone();
            continue;
        }
        let event = websocket_event_type(&raw);
        if event.as_deref() == Some("response.completed") {
            match websocket_response_completed_id(&raw) {
                Ok(Some(response_id)) => continuation.record_completed(response_id),
                Ok(None) => unreachable!("event type was checked"),
                Err(message) => {
                    discard_stream_websocket(
                        websocket,
                        pool_return,
                        StreamWebSocketDiscardReason::InvalidCompletedResponse,
                    )
                    .await;
                    let _ = tx
                        .send(Err(CodexWebSocketExchangeError::InvalidCompletedResponse {
                            message,
                        }))
                        .await;
                    return;
                }
            }
        }
        let Some(frame) = websocket_event_to_sse_frame(&raw) else {
            continue;
        };
        let terminal = event.as_deref().is_some_and(is_terminal_websocket_event);
        if tx.send(Ok(Bytes::from(frame))).await.is_err() {
            discard_stream_websocket(
                websocket,
                pool_return,
                StreamWebSocketDiscardReason::DownstreamSendFailed,
            )
            .await;
            return;
        }
        if terminal {
            match event.as_deref() {
                Some("response.completed") => {
                    finish_stream_websocket(websocket, metadata, continuation, pool_return.take())
                        .await;
                }
                Some("response.incomplete") => {
                    discard_stream_websocket(
                        websocket,
                        pool_return,
                        StreamWebSocketDiscardReason::IncompleteResponse,
                    )
                    .await;
                }
                Some("response.failed" | "error") => {
                    discard_stream_websocket(
                        websocket,
                        pool_return,
                        StreamWebSocketDiscardReason::FailedResponse,
                    )
                    .await;
                }
                _ => unreachable!("terminal websocket event was matched above"),
            }
            return;
        }
    }

    discard_stream_websocket(
        websocket,
        pool_return,
        StreamWebSocketDiscardReason::UpstreamClosed,
    )
    .await;
    let error = CodexWebSocketExchangeError::ClosedBeforeTerminal;
    let error = if reused_connection {
        reused_stream_receive_error(error)
    } else {
        error
    };
    let _ = tx.send(Err(error)).await;
}

fn receive_idle_timeout(
    saw_upstream_activity: bool,
    initial_event_timeout: Option<Duration>,
) -> Duration {
    if saw_upstream_activity {
        WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT
    } else {
        initial_event_timeout
            .filter(|timeout| !timeout.is_zero())
            .unwrap_or(WEBSOCKET_RECEIVE_IDLE_TIMEOUT)
    }
}

async fn next_websocket_message(
    websocket: &mut PumpedWebSocket,
    receive_timeout: Duration,
) -> Result<Option<Message>, CodexWebSocketExchangeError> {
    match timeout(receive_timeout, websocket.next()).await {
        Ok(message) => message.transpose().map_err(Into::into),
        Err(_) => Err(CodexWebSocketExchangeError::ReceiveIdleTimeout {
            timeout: receive_timeout,
        }),
    }
}

async fn finish_stream_websocket(
    websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    continuation: WebSocketContinuationState,
    pool_return: Option<WebSocketStreamPoolReturn>,
) {
    let Some(pool_return) = pool_return else {
        websocket.close().await;
        return;
    };
    pool_return
        .lease
        .put(PooledWebSocketConnection {
            websocket,
            metadata: reusable_websocket_metadata(metadata),
            continuation,
            created_at: pool_return.created_at,
        })
        .await;
}

async fn discard_stream_websocket(
    websocket: PumpedWebSocket,
    pool_return: Option<WebSocketStreamPoolReturn>,
    reason: StreamWebSocketDiscardReason,
) {
    let websocket_connection_id = websocket.connection_id();
    let pump_exit = websocket.exit_reason();
    let pump_exit_reason = pump_exit
        .as_ref()
        .map(PumpExitReason::as_str)
        .unwrap_or("running");
    let pump_exit_detail = pump_exit
        .as_ref()
        .and_then(PumpExitReason::detail)
        .unwrap_or_default();
    tracing::info!(
        websocket_connection_id = %websocket_connection_id,
        reason = reason.as_str(),
        pump_exit_reason,
        pump_exit_detail,
        pooled = pool_return.is_some(),
        "Discarding Responses WebSocket stream"
    );
    if let Some(pool_return) = pool_return {
        pool_return.lease.discard().await;
    }
    websocket.close().await;
}

// ---------------------------------------------------------------------------
// Header / metadata helpers
// ---------------------------------------------------------------------------

fn websocket_rate_limit_event_headers(raw: &str) -> Option<Vec<(String, String)>> {
    events::parse_rate_limits_event_raw(raw)
        .map(|parsed| events::rate_limits_to_header_pairs(&parsed))
}

fn update_websocket_response_metadata(metadata: &mut CodexWebSocketConnectionMetadata, raw: &str) {
    response_meta::merge_response_metadata(
        &mut metadata.response_metadata,
        websocket_metadata_headers(raw),
    );
}

pub(super) fn audit_header_value(name: &str, value: &str) -> String {
    if is_sensitive_opening_header(name) {
        REDACTED_HEADER_VALUE.to_string()
    } else {
        value.to_string()
    }
}

fn is_sensitive_opening_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "chatgpt-account-id"
            | "cookie"
            | "session_id"
            | "session-id"
            | "thread-id"
            | "x-client-request-id"
            | "x-codex-window-id"
            | "x-codex-turn-metadata"
            | "x-codex-turn-state"
            | "x-codex-parent-thread-id"
    )
}

pub(super) fn websocket_audit_file_name() -> String {
    let timestamp = china_filename_timestamp_millis(&Utc::now());
    format!("codex-ws-audit-{timestamp}-{}.json", Uuid::new_v4())
}
