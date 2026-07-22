//! Codex HTTP/SSE/WebSocket 上游 transport。

pub mod canonical;
pub mod catalog;
pub mod client;
mod client_sse;
pub mod diagnostics;
pub mod endpoints;
pub mod headers;
pub mod profile;
pub mod protocol;
pub mod request;
mod response_meta;
mod time;
pub mod tls;
pub mod usage;
pub mod websocket;

pub use self::{
    canonical::{CodexCanonicalDecoder, CodexCanonicalError},
    catalog::{
        CodexCatalogCapabilities, CodexCatalogCapabilityEvidence, CodexCatalogLimits,
        CodexCatalogMetadata, CodexCatalogModel, CodexCatalogVisibility, CodexModelCatalogError,
        CodexModelCatalogSnapshot, MAX_CODEX_MODEL_CATALOG_BYTES, parse_codex_model_catalog,
    },
    client::{
        CodexBackendClient, CodexBackendResponse, CodexBackendSseStream,
        CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError, CodexClientResult,
        CodexRateLimitHeaderUpdates, CodexRequestContext, CodexTransportDecision,
        CodexTransportMetrics, CodexTurnStateUpdate, build_reqwest_client,
    },
    diagnostics::{CodexUpstreamDiagnostics, CodexUpstreamSendPhase},
    endpoints::{
        CODEX_RESPONSES_PATH, CODEX_USAGE_API_PATH, CODEX_USAGE_PATH, WHAM_USAGE_PATH,
        endpoint_url, usage_endpoint_urls,
    },
    headers::build_codex_base_headers,
    request::{CodexRequestEncodeError, encode_generate_request},
    response_meta::CodexResponseMetadata,
    usage::{MAX_CODEX_USAGE_BODY_BYTES, openai_billing_breakdown},
    websocket::{
        CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey, WebSocketPoolDecision,
    },
};
