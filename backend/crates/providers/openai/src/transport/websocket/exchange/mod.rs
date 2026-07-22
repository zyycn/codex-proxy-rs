//! WebSocket 帧交换、流转发与结果类型。

mod io;
mod reducer;
mod stream;

use std::{pin::Pin, sync::Arc, time::Duration};

use bytes::Bytes;
use futures::Stream;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::transport::{
    diagnostics::CodexUpstreamDiagnostics, response_meta::CodexResponseMetadata,
};

use super::error::CodexWebSocketExchangeError;
use super::pool::{CodexWebSocketConnectionMetadata, WebSocketPoolDecision};

const WEBSOCKET_RECEIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_STREAM_BUFFER: usize = 16;

pub(super) use self::stream::{WebSocketStreamPoolReturn, stream_websocket_response};

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

pub(super) fn reusable_websocket_metadata(
    mut metadata: CodexWebSocketConnectionMetadata,
) -> CodexWebSocketConnectionMetadata {
    metadata.rate_limit_headers.clear();
    metadata
}
