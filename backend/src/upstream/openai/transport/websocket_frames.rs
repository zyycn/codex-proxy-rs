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
    /// 复用的池连接在首个真实输出前失效。
    #[error("reused websocket connection died before first output: {message}")]
    ReusedConnectionDiedBeforeFirstOutput {
        /// 底层失效原因。
        message: String,
    },
    /// 首个真实输出在配置的绝对超时内未到达（连接落到病态上游后端）。
    #[error("websocket first output not received within {timeout:?}")]
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
    reused_connection: bool,
    started_at: Instant,
    first_token_started_at: Instant,
    first_token_timeout: Option<Duration>,
) -> Result<
    (
        CodexWebSocketExchange,
        PumpedWebSocket,
        CodexWebSocketConnectionMetadata,
    ),
    CodexWebSocketExchangeError,
> {
    let mut body = String::new();
    let mut saw_first_output = false;
    let mut first_token_ms = None;

    loop {
        let receive_timeout = receive_idle_timeout_with_first_token_deadline(
            saw_first_output,
            first_token_started_at,
            first_token_timeout,
        )?;
        let message = match next_websocket_message(&mut websocket, receive_timeout).await {
            Ok(message) => message,
            Err(error) if !saw_first_output => {
                if let Some(timeout) =
                    elapsed_first_token_timeout(first_token_started_at, first_token_timeout)
                {
                    return Err(CodexWebSocketExchangeError::FirstTokenTimeout { timeout });
                }
                if reused_connection {
                    return Err(reused_connection_died_before_first_output(&error));
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let Some(message) = message else {
            break;
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(_) => return Err(CodexWebSocketExchangeError::UnexpectedBinaryEvent),
            Message::Close(_) if reused_connection && !saw_first_output => {
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
        if let Some(classified) = classify_websocket_error_frame(&raw) {
            let retry_after_seconds = retry_after_seconds_from_wrapped_error_headers(&raw)
                .or_else(|| events::retry_after_seconds_from_body(&raw));
            return Err(CodexWebSocketExchangeError::upstream(
                classified.status_code,
                retry_after_seconds,
                raw,
                Vec::new(),
                CodexUpstreamDiagnostics::with_status(classified.status_code),
            ));
        }
        if let Some(reason) = websocket_incomplete_response_reason(&raw) {
            return Err(CodexWebSocketExchangeError::IncompleteResponse { reason });
        }
        if let Some(message) = websocket_response_completed_parse_error(&raw) {
            return Err(CodexWebSocketExchangeError::InvalidCompletedResponse { message });
        }
        if let Some(headers) = websocket_rate_limit_event_headers(&raw) {
            metadata.rate_limit_headers.extend(headers);
            continue;
        }
        if let Some(metadata_turn_state) = websocket_metadata_turn_state(&raw) {
            metadata.turn_state = Some(metadata_turn_state);
            continue;
        }
        let event = websocket_event_type(&raw);
        let forwarded = if let Some(frame) = websocket_event_to_sse_frame(&raw) {
            let has_first_output = response_body_has_first_output(frame.as_bytes());
            body.push_str(&frame);
            if has_first_output {
                saw_first_output = true;
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
        if forwarded && event.as_deref().is_some_and(is_terminal_websocket_event) {
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
            };
            return Ok((exchange, websocket, metadata));
        }
    }

    if reused_connection && !saw_first_output {
        return Err(
            CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                message: "websocket closed before terminal event".to_string(),
            },
        );
    }

    Err(CodexWebSocketExchangeError::ClosedBeforeTerminal)
}

pub(super) async fn prefetch_stream_frames_until_output_or_terminal(
    websocket: &mut PumpedWebSocket,
    metadata: &mut CodexWebSocketConnectionMetadata,
    first_token_started_at: Instant,
    first_token_timeout: Option<Duration>,
) -> Result<Vec<String>, CodexWebSocketExchangeError> {
    let mut prefetched_frames = Vec::new();
    loop {
        let receive_timeout =
            receive_timeout_before_first_output(first_token_started_at, first_token_timeout)?;
        let message = match next_websocket_message(websocket, receive_timeout).await {
            Ok(Some(message)) => message,
            Ok(None) => return Err(CodexWebSocketExchangeError::ClosedBeforeTerminal),
            Err(error) => {
                if let Some(timeout) =
                    elapsed_first_token_timeout(first_token_started_at, first_token_timeout)
                {
                    return Err(CodexWebSocketExchangeError::FirstTokenTimeout { timeout });
                }
                return Err(error);
            }
        };

        let raw = match message {
            Message::Text(text) => text.to_string(),
            Message::Binary(_) => return Err(CodexWebSocketExchangeError::UnexpectedBinaryEvent),
            Message::Close(_) => return Err(CodexWebSocketExchangeError::ClosedBeforeTerminal),
            _ => continue,
        };

        if let Some(classified) = classify_websocket_error_frame(&raw) {
            let retry_after_seconds = retry_after_seconds_from_wrapped_error_headers(&raw)
                .or_else(|| events::retry_after_seconds_from_body(&raw));
            return Err(CodexWebSocketExchangeError::upstream(
                classified.status_code,
                retry_after_seconds,
                raw,
                Vec::new(),
                CodexUpstreamDiagnostics::with_status(classified.status_code),
            ));
        }
        if let Some(reason) = websocket_incomplete_response_reason(&raw) {
            return Err(CodexWebSocketExchangeError::IncompleteResponse { reason });
        }
        if let Some(message) = websocket_response_completed_parse_error(&raw) {
            return Err(CodexWebSocketExchangeError::InvalidCompletedResponse { message });
        }
        if let Some(headers) = websocket_rate_limit_event_headers(&raw) {
            metadata.rate_limit_headers.extend(headers);
            continue;
        }
        if let Some(metadata_turn_state) = websocket_metadata_turn_state(&raw) {
            metadata.turn_state = Some(metadata_turn_state);
            continue;
        }

        let event = websocket_event_type(&raw);
        let Some(frame) = websocket_event_to_sse_frame(&raw) else {
            continue;
        };
        let has_first_output = response_body_has_first_output(frame.as_bytes());
        let terminal = event.as_deref().is_some_and(is_terminal_websocket_event);
        prefetched_frames.push(raw);
        if has_first_output || terminal {
            return Ok(prefetched_frames);
        }
    }
}

pub(super) fn reused_stream_prefetch_error(
    error: CodexWebSocketExchangeError,
) -> CodexWebSocketExchangeError {
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
    pub(super) pool: CodexWebSocketPool,
    pub(super) key: CodexWebSocketPoolKey,
    pub(super) created_at: Instant,
}

#[derive(Debug, Clone, Copy)]
enum StreamWebSocketDiscardReason {
    ClientCancelled,
    DownstreamSendFailed,
    IncompleteResponse,
    InvalidCompletedResponse,
    UnexpectedBinaryEvent,
    UpstreamClosed,
    UpstreamError,
    UpstreamReceiveFailed,
}

impl StreamWebSocketDiscardReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClientCancelled => "client_cancelled",
            Self::DownstreamSendFailed => "downstream_send_failed",
            Self::IncompleteResponse => "incomplete_response",
            Self::InvalidCompletedResponse => "invalid_completed_response",
            Self::UnexpectedBinaryEvent => "unexpected_binary_event",
            Self::UpstreamClosed => "upstream_closed",
            Self::UpstreamError => "upstream_error",
            Self::UpstreamReceiveFailed => "upstream_receive_failed",
        }
    }
}

pub(super) fn stream_websocket_response(
    websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
    prefetched_frames: Vec<String>,
) -> CodexWebSocketStreamingExchange {
    let response_metadata = metadata.clone();
    let rate_limit_header_updates = Arc::new(Mutex::new(Vec::new()));
    let rate_limit_header_updates_for_task = Arc::clone(&rate_limit_header_updates);
    let turn_state_update = Arc::new(Mutex::new(metadata.turn_state.clone()));
    let turn_state_update_for_task = Arc::clone(&turn_state_update);
    let (tx, rx) = mpsc::channel(WEBSOCKET_STREAM_BUFFER);
    tokio::spawn(async move {
        forward_websocket_response_stream(
            websocket,
            metadata,
            pool_return,
            prefetched_frames,
            rate_limit_header_updates_for_task,
            turn_state_update_for_task,
            tx,
        )
        .await;
    });

    let body = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });

    CodexWebSocketStreamingExchange {
        body: Box::pin(body),
        turn_state: response_metadata.turn_state,
        set_cookie_headers: response_metadata.set_cookie_headers,
        rate_limit_headers: response_metadata.rate_limit_headers,
        rate_limit_header_updates,
        turn_state_update,
        pool_decision: None,
        diagnostics: response_metadata.diagnostics,
    }
}

async fn forward_websocket_response_stream(
    mut websocket: PumpedWebSocket,
    mut metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
    prefetched_frames: Vec<String>,
    rate_limit_header_updates: CodexWebSocketRateLimitHeaderUpdates,
    turn_state_update: CodexWebSocketTurnStateUpdate,
    tx: mpsc::Sender<Result<Bytes, CodexWebSocketExchangeError>>,
) {
    let mut pool_return = pool_return;
    let mut prefetched_frames: VecDeque<String> = prefetched_frames.into();
    let mut saw_first_output = false;
    loop {
        let raw = if let Some(text) = prefetched_frames.pop_front() {
            text
        } else {
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
                    receive_idle_timeout(saw_first_output),
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
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            };
            let Some(message) = message else {
                break;
            };
            match message {
                Message::Text(text) => text.to_string(),
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
                    let _ = tx
                        .send(Err(CodexWebSocketExchangeError::ClosedBeforeTerminal))
                        .await;
                    return;
                }
                _ => continue,
            }
        };
        if let Some(classified) = classify_websocket_error_frame(&raw) {
            discard_stream_websocket(
                websocket,
                pool_return,
                StreamWebSocketDiscardReason::UpstreamError,
            )
            .await;
            let retry_after_seconds = retry_after_seconds_from_wrapped_error_headers(&raw)
                .or_else(|| events::retry_after_seconds_from_body(&raw));
            let _ = tx
                .send(Err(CodexWebSocketExchangeError::upstream(
                    classified.status_code,
                    retry_after_seconds,
                    raw,
                    Vec::new(),
                    CodexUpstreamDiagnostics::with_status(classified.status_code),
                )))
                .await;
            return;
        }
        if let Some(reason) = websocket_incomplete_response_reason(&raw) {
            discard_stream_websocket(
                websocket,
                pool_return,
                StreamWebSocketDiscardReason::IncompleteResponse,
            )
            .await;
            let _ = tx
                .send(Err(CodexWebSocketExchangeError::IncompleteResponse {
                    reason,
                }))
                .await;
            return;
        }
        if let Some(message) = websocket_response_completed_parse_error(&raw) {
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
        if let Some(headers) = websocket_rate_limit_event_headers(&raw) {
            metadata.rate_limit_headers.extend(headers.iter().cloned());
            rate_limit_header_updates.lock().await.extend(headers);
            continue;
        }
        if let Some(metadata_turn_state) = websocket_metadata_turn_state(&raw) {
            metadata.turn_state = Some(metadata_turn_state);
            *turn_state_update.lock().await = metadata.turn_state.clone();
            continue;
        }
        let event = websocket_event_type(&raw);
        let Some(frame) = websocket_event_to_sse_frame(&raw) else {
            continue;
        };
        let terminal = event.as_deref().is_some_and(is_terminal_websocket_event);
        let has_first_output = response_body_has_first_output(frame.as_bytes());
        if tx.send(Ok(Bytes::from(frame))).await.is_err() {
            discard_stream_websocket(
                websocket,
                pool_return,
                StreamWebSocketDiscardReason::DownstreamSendFailed,
            )
            .await;
            return;
        }
        if has_first_output {
            saw_first_output = true;
        }
        if terminal {
            finish_stream_websocket(websocket, metadata, pool_return.take()).await;
            return;
        }
    }

    discard_stream_websocket(
        websocket,
        pool_return,
        StreamWebSocketDiscardReason::UpstreamClosed,
    )
    .await;
    let _ = tx
        .send(Err(CodexWebSocketExchangeError::ClosedBeforeTerminal))
        .await;
}

fn receive_idle_timeout(saw_response_frame: bool) -> Duration {
    if saw_response_frame {
        WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT
    } else {
        WEBSOCKET_RECEIVE_IDLE_TIMEOUT
    }
}

fn receive_idle_timeout_with_first_token_deadline(
    saw_first_output: bool,
    first_token_started_at: Instant,
    first_token_timeout: Option<Duration>,
) -> Result<Duration, CodexWebSocketExchangeError> {
    if saw_first_output {
        return Ok(WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT);
    }
    receive_timeout_before_first_output(first_token_started_at, first_token_timeout)
}

fn receive_timeout_before_first_output(
    first_token_started_at: Instant,
    first_token_timeout: Option<Duration>,
) -> Result<Duration, CodexWebSocketExchangeError> {
    let Some(first_token_timeout) = first_token_timeout.filter(|timeout| !timeout.is_zero()) else {
        return Ok(WEBSOCKET_RECEIVE_IDLE_TIMEOUT);
    };
    let Some(remaining) = first_token_timeout.checked_sub(first_token_started_at.elapsed()) else {
        return Err(CodexWebSocketExchangeError::FirstTokenTimeout {
            timeout: first_token_timeout,
        });
    };
    if remaining.is_zero() {
        return Err(CodexWebSocketExchangeError::FirstTokenTimeout {
            timeout: first_token_timeout,
        });
    }
    Ok(WEBSOCKET_RECEIVE_IDLE_TIMEOUT.min(remaining))
}

fn elapsed_first_token_timeout(
    first_token_started_at: Instant,
    first_token_timeout: Option<Duration>,
) -> Option<Duration> {
    let first_token_timeout = first_token_timeout.filter(|timeout| !timeout.is_zero())?;
    (first_token_started_at.elapsed() >= first_token_timeout).then_some(first_token_timeout)
}

pub(super) fn is_first_token_timeout(error: &CodexWebSocketExchangeError) -> bool {
    matches!(error, CodexWebSocketExchangeError::FirstTokenTimeout { .. })
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
    mut websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
) {
    let Some(pool_return) = pool_return else {
        websocket.close().await;
        return;
    };
    pool_return
        .pool
        .put(
            pool_return.key,
            PooledWebSocketConnection {
                websocket,
                metadata: reusable_websocket_metadata(metadata),
                created_at: pool_return.created_at,
            },
        )
        .await;
}

async fn discard_stream_websocket(
    mut websocket: PumpedWebSocket,
    pool_return: Option<WebSocketStreamPoolReturn>,
    reason: StreamWebSocketDiscardReason,
) {
    tracing::info!(
        reason = reason.as_str(),
        pooled = pool_return.is_some(),
        "discarding Responses WebSocket stream"
    );
    if let Some(pool_return) = pool_return {
        pool_return.pool.discard(&pool_return.key).await;
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
