//! WebSocket 帧交换、流转发与结果类型。

mod collect;
mod io;
mod reducer;
mod stream;

use std::{
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::Stream;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::upstream::openai::{
    protocol::events::TokenUsage,
    transport::{diagnostics::CodexUpstreamDiagnostics, response_meta::CodexResponseMetadata},
};

use super::breaker::WebSocketOriginBreaker;
use super::error::CodexWebSocketExchangeError;
use super::pool::{CodexWebSocketConnectionMetadata, WebSocketPoolDecision};
use super::{
    CodexWebSocketRequest, execute_prepared_response_create_request,
    prepare_response_create_request_with_pool,
};

const WEBSOCKET_RECEIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_STREAM_BUFFER: usize = 16;

pub(super) use self::{
    collect::{CollectedWebSocket, collect_websocket_response},
    reducer::WebSocketTerminalKind,
    stream::{WebSocketStreamPoolReturn, stream_websocket_response},
};

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
    /// 首个 reasoning 输出事件到达代理的耗时。
    pub first_reasoning_ms: Option<i64>,
    /// 首个正文输出事件到达代理的耗时。
    pub first_text_ms: Option<i64>,
    /// 首个上游协议事件到达代理的耗时。
    pub first_event_ms: Option<i64>,
    /// WebSocket 连接池决策。
    pub pool_decision: Option<WebSocketPoolDecision>,
    /// terminal completed 后该 socket 是否会保留 connection-local continuation。
    pub connection_local_continuation: bool,
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
    /// terminal completed 后该 socket 是否会保留 connection-local continuation。
    pub connection_local_continuation: bool,
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

/// 执行一次 prepared Responses WebSocket 请求并聚合为 SSE。
pub async fn execute_response_create_request(
    request: &CodexWebSocketRequest,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let breaker = WebSocketOriginBreaker::default();
    let prepared = prepare_response_create_request_with_pool(
        request,
        None,
        &breaker,
        request.connection().endpoint(),
        None,
        false,
        None,
    )
    .await?;
    execute_prepared_response_create_request(request, prepared, Instant::now()).await
}

pub(super) fn reusable_websocket_metadata(
    mut metadata: CodexWebSocketConnectionMetadata,
) -> CodexWebSocketConnectionMetadata {
    metadata.rate_limit_headers.clear();
    metadata
}
