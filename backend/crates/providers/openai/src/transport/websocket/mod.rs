//! Codex Responses WebSocket 传输

mod audit;
mod breaker;
mod control;
mod coordinator;
mod error;
mod exchange;
mod handshake;
mod live;
mod model;
mod pool;
mod pump;

pub use self::{
    audit::{
        WS_AUDIT_DIR_ENV, websocket_audit_dir, write_websocket_audit_artifact_for_dir,
        write_websocket_audit_artifact_from_env,
    },
    breaker::{
        WebSocketOriginBreaker, WebSocketOriginBreakerConfig, WebSocketOriginBreakerDecision,
        WebSocketOriginBreakerPermit,
    },
    error::{CodexWebSocketCloseError, CodexWebSocketExchangeError, CodexWebSocketUpstreamError},
    exchange::{
        CodexWebSocketRateLimitUpdates, CodexWebSocketResponseMetadataUpdates,
        CodexWebSocketStreamingExchange, CodexWebSocketTextStream,
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
    pump::WebSocketConnectionObservation,
};
pub(crate) use self::{
    coordinator::{
        PreparedWebSocket, WebSocketFastPath, execute_prepared_response_create_request_stream,
        post_send_ambiguous, prepare_response_create_request_with_pool,
    },
    error::WEBSOCKET_CLOSE_MESSAGE_TOO_BIG,
    handshake::WEBSOCKET_CONNECT_TIMEOUT,
    live::{connect_live_sideband, into_live_relay},
    pool::{DEFAULT_FAST_PATH_BUDGET_MS, DEFAULT_STREAM_IDLE_TIMEOUT},
};
