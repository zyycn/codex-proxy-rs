//! Codex HTTP/SSE/WebSocket 上游 transport。

pub mod canonical;
pub mod catalog;
pub mod client;
mod client_json;
mod client_sse;
pub mod diagnostics;
pub mod endpoints;
pub mod headers;
pub mod profile;
pub mod profile_avatar;
pub mod profile_statistics;
pub mod protocol;
pub mod request;
pub mod reset_credits;
mod response_meta;
pub(crate) mod session;
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
        CodexAccountSelectionTelemetry, CodexBackendClient, CodexBackendJsonResponse,
        CodexBackendSseStream, CodexBackendStreamingResponse, CodexBackendTransport,
        CodexClientError, CodexClientResult, CodexRateLimitUpdates, CodexRequestContext,
        CodexTransportDecision, CodexTransportMetrics, CodexTurnStateUpdate, build_reqwest_client,
    },
    diagnostics::{CodexUpstreamDiagnostics, CodexUpstreamSendPhase},
    endpoints::{
        CODEX_ALPHA_SEARCH_PATH, CODEX_IMAGE_EDITS_PATH, CODEX_IMAGE_GENERATIONS_PATH,
        CODEX_RESPONSES_PATH, CODEX_USAGE_API_PATH, WHAM_PROFILE_STATISTICS_PATH,
        WHAM_RATE_LIMIT_RESET_CREDITS_CONSUME_PATH, WHAM_RATE_LIMIT_RESET_CREDITS_PATH,
        WHAM_USAGE_PATH, endpoint_url, usage_endpoint_url,
    },
    headers::build_codex_base_headers,
    profile_avatar::{
        CodexProfileAvatar, CodexProfileAvatarFetchError, CodexProfileAvatarStreamError,
        fetch_profile_avatar,
    },
    profile_statistics::{
        CodexProfileActivityInsights, CodexProfileDailyUsage, CodexProfileInvocation,
        CodexProfileStatistics, CodexProfileStatisticsSummary,
        MAX_CODEX_PROFILE_STATISTICS_BODY_BYTES,
    },
    request::{CodexRequestEncodeError, encode_generate_request},
    reset_credits::{
        CodexRateLimitResetCredit, CodexRateLimitResetCredits,
        CodexRateLimitResetCreditsConsumeResult, MAX_CODEX_RESET_CREDITS_BODY_BYTES,
    },
    response_meta::CodexResponseMetadata,
    usage::{MAX_CODEX_USAGE_BODY_BYTES, OpenAiBillingUsage, openai_billing_breakdown},
    websocket::{
        CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey, WebSocketPoolDecision,
    },
};
