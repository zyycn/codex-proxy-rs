//! Codex HTTP/SSE/WebSocket 上游 transport。

pub mod client;
pub mod endpoints;
pub mod headers;
pub mod tls;
pub mod usage;
pub mod websocket;
pub mod websocket_pool;

pub use self::{
    client::{
        backend_transport_for_response_request, build_reqwest_client, is_banned_auth_signal,
        is_banned_upstream_error, CodexBackendClient, CodexBackendResponse, CodexBackendSseStream,
        CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError, CodexClientResult,
        CodexCompactResponse, CodexConnectivityProbe, CodexModelCatalogClient,
        CodexModelCatalogClientError, CodexModelCatalogRequest, CodexRateLimitHeaderUpdates,
        CodexRequestContext, CodexTurnStateUpdate,
    },
    endpoints::{
        endpoint_request_path, endpoint_url, primary_usage_request_path, usage_endpoint_urls,
        CODEX_RESPONSES_COMPACT_PATH, CODEX_RESPONSES_PATH, CODEX_USAGE_API_PATH, CODEX_USAGE_PATH,
        WHAM_USAGE_PATH,
    },
    headers::{
        build_codex_base_headers, build_codex_headers, build_ordered_codex_base_headers,
        build_ordered_codex_headers, order_headers,
    },
    tls::{
        build_reqwest_client_with_custom_ca, custom_ca_env_cache_key,
        maybe_build_rustls_client_config_with_custom_ca, CustomCaError, CustomCaResult,
        CODEX_CA_CERT_ENV, SSL_CERT_FILE_ENV,
    },
    websocket_pool::{CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey},
};
