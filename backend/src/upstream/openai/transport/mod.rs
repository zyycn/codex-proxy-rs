//! Codex HTTP/SSE/WebSocket 上游 transport。

pub mod client;
pub mod diagnostics;
pub mod endpoints;
pub mod headers;
mod response_meta;
pub mod tls;
pub mod usage;
pub mod websocket;

pub(crate) use self::client::PreparedResponseTransport;
pub use self::{
    client::{
        CodexBackendClient, CodexBackendResponse, CodexBackendSseStream,
        CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError, CodexClientResult,
        CodexRateLimitHeaderUpdates, CodexRequestContext, CodexTransportDecision,
        CodexTransportMetrics, CodexTurnStateUpdate, build_reqwest_client,
    },
    diagnostics::CodexUpstreamDiagnostics,
    endpoints::{
        CODEX_RESPONSES_PATH, CODEX_USAGE_API_PATH, CODEX_USAGE_PATH, WHAM_USAGE_PATH,
        endpoint_request_path, endpoint_url, usage_endpoint_urls,
    },
    headers::{build_codex_base_headers, build_codex_headers},
    response_meta::CodexResponseMetadata,
    websocket::{
        CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey, WebSocketPoolDecision,
    },
};
