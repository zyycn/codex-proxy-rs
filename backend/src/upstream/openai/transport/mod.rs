//! Codex HTTP/SSE/WebSocket 上游 transport。

pub mod client;
pub mod diagnostics;
pub mod endpoints;
pub mod headers;
mod response_meta;
pub mod tls;
pub mod usage;
pub mod websocket;
pub mod websocket_pool;
pub(crate) mod websocket_pump;

pub use self::{
    client::{
        CodexBackendClient, CodexBackendResponse, CodexBackendSseStream,
        CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError, CodexClientResult,
        CodexModelCatalogClient, CodexModelCatalogClientError, CodexModelCatalogRequest,
        CodexRateLimitHeaderUpdates, CodexRequestContext, CodexResponseMetadata,
        CodexTurnStateUpdate, backend_transport_for_response_request, build_reqwest_client,
        is_banned_auth_signal, is_banned_upstream_error, is_cyber_policy_error_body,
        is_cyber_policy_upstream_error, is_deactivated_workspace_error_body,
    },
    diagnostics::CodexUpstreamDiagnostics,
    endpoints::{
        CODEX_RESPONSES_PATH, CODEX_USAGE_API_PATH, CODEX_USAGE_PATH, WHAM_USAGE_PATH,
        endpoint_request_path, endpoint_url, usage_endpoint_urls,
    },
    headers::{
        build_codex_base_headers, build_codex_headers, build_ordered_codex_base_headers,
        build_ordered_codex_headers, order_headers,
    },
    websocket_pool::{
        CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey, WebSocketPoolDecision,
    },
};
