//! WebSocket 帧交换、流转发与结果类型

mod io;
mod reducer;
mod stream;

use std::{pin::Pin, sync::Arc};

use bytes::Bytes;
use futures::Stream;
use gateway_protocol::openai::events::ParsedRateLimits;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::transport::{
    diagnostics::CodexUpstreamDiagnostics, response_meta::CodexResponseMetadata,
};

use super::error::CodexWebSocketExchangeError;
use super::pool::{CodexWebSocketConnectionMetadata, WebSocketPoolDecision};

const WEBSOCKET_STREAM_BUFFER: usize = 16;

pub(super) use self::stream::{WebSocketStreamPoolReturn, stream_websocket_response};

/// Responses WebSocket 文本流交互结果
pub struct CodexWebSocketStreamingExchange {
    pub(crate) routing_affinity: Option<crate::transport::routing_affinity::ChatGptRoutingAffinity>,
    /// 关联请求日志与底层 pump 生命周期日志的连接标识
    pub(crate) websocket_connection_id: Uuid,
    /// 上游完整 WebSocket 文本消息流
    pub body: CodexWebSocketTextStream,
    /// 上游为本次响应返回的首个 turn state
    pub turn_state: Option<String>,
    /// 上游握手响应里的 `set-cookie` 列表
    pub set_cookie_headers: Vec<String>,
    /// 上游握手响应里的限流头
    pub rate_limit_headers: Vec<(String, String)>,
    /// 上游内部 `codex.rate_limits` 事件里的结构化动态更新
    pub rate_limit_updates: CodexWebSocketRateLimitUpdates,
    /// 上游内部 metadata 事件里的请求级动态更新
    pub response_metadata_updates: CodexWebSocketResponseMetadataUpdates,
    /// WebSocket 连接池决策
    pub pool_decision: Option<WebSocketPoolDecision>,
    /// terminal completed 后该 socket 是否会保留 connection-local continuation
    pub connection_local_continuation: bool,
    /// 上游诊断元数据
    pub diagnostics: CodexUpstreamDiagnostics,
    /// 安全响应元数据
    pub response_metadata: CodexResponseMetadata,
}

/// Responses WebSocket 完整文本消息流
pub type CodexWebSocketTextStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, CodexWebSocketExchangeError>> + Send + 'static>>;
/// live 流中的结构化限流动态更新
pub type CodexWebSocketRateLimitUpdates = Arc<Mutex<Vec<ParsedRateLimits>>>;
/// 单次响应的动态 metadata，与连接池保存的握手快照隔离
#[derive(Debug, Default)]
pub struct CodexWebSocketResponseMetadataUpdate {
    /// 当前响应首次声明的会话续接状态
    pub turn_state: Option<String>,
}

pub type CodexWebSocketResponseMetadataUpdates = Arc<Mutex<CodexWebSocketResponseMetadataUpdate>>;

pub(super) fn reusable_websocket_metadata(
    mut metadata: CodexWebSocketConnectionMetadata,
) -> CodexWebSocketConnectionMetadata {
    // 握手 Cookie 只属于首次响应，复用旧头会重复写入并覆盖后续轮换结果
    metadata.set_cookie_headers.clear();
    metadata.rate_limit_headers.clear();
    metadata.turn_state = None;
    // 请求级响应头不能随池中连接带到下一轮响应
    metadata
        .response_metadata
        .client_headers
        .retain(|(name, _)| {
            !["x-codex-turn-state", "openai-model", "x-openai-model"]
                .iter()
                .any(|header| name.eq_ignore_ascii_case(header))
        });
    metadata
}
