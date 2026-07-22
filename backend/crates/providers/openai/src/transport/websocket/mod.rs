//! Codex HTTP Responses WebSocket transport。

mod audit;
mod breaker;
mod coordinator;
mod error;
mod exchange;
mod handshake;
mod model;
mod pool;
mod pump;

pub use self::{
    audit::{
        WS_AUDIT_DIR_ENV, write_websocket_audit_artifact_for_dir,
        write_websocket_audit_artifact_from_env,
    },
    breaker::{
        WebSocketOriginBreaker, WebSocketOriginBreakerConfig, WebSocketOriginBreakerDecision,
        WebSocketOriginBreakerPermit,
    },
    error::{CodexWebSocketExchangeError, CodexWebSocketUpstreamError},
    exchange::{
        CodexWebSocketRateLimitHeaderUpdates, CodexWebSocketSseStream,
        CodexWebSocketStreamingExchange, CodexWebSocketTurnStateUpdate,
    },
    handshake::responses_websocket_endpoint,
    model::{
        CodexWebSocketConnection, CodexWebSocketRequest, PreviousResponseUnavailableReason,
        WebSocketContinuationRequirement,
    },
    pool::{
        CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey,
        WebSocketPoolBypassReason, WebSocketPoolDecision,
    },
};
pub(crate) use self::{
    coordinator::{
        PreparedWebSocket, WEBSOCKET_FAST_PATH_BUDGET,
        execute_prepared_response_create_request_stream, post_send_ambiguous,
        prepare_response_create_request_with_pool,
    },
    pool::DEFAULT_INITIAL_EVENT_TIMEOUT,
};
