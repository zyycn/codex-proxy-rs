//! xAI 上游 HTTP、SSE、请求编码与会话传输的模块门面

pub(crate) mod canonical;
pub(crate) mod catalog;
pub(crate) mod compaction;
pub(crate) mod config;
pub(crate) mod egress;
pub(crate) mod headers;
mod inference;
pub(crate) mod network;
mod request;
mod session;
mod upstream_error;

pub use canonical::GrokCanonicalDecoder;
pub use catalog::{
    GROK_BILLING_URL, GROK_MODEL_CATALOG_URL, GROK_SUBSCRIPTION_URL, GrokBillingClient,
    GrokBillingError, GrokBillingRequest, GrokBillingSnapshot, GrokBillingTransport,
    GrokBillingTransportError, GrokBillingTransportErrorKind, GrokBillingTransportFuture,
    GrokBillingTransportResponse, GrokCatalogApiBackend, GrokCatalogCapabilities,
    GrokCatalogCapabilityEvidence, GrokCatalogLimits, GrokCatalogMetadata, GrokCatalogModel,
    GrokCatalogReasoningEffort, GrokModelCatalogClient, GrokModelCatalogError,
    GrokModelCatalogRequest, GrokModelCatalogSession, GrokModelCatalogSessionError,
    GrokModelCatalogSnapshot, GrokModelCatalogTransport, GrokModelCatalogTransportError,
    GrokModelCatalogTransportErrorKind, GrokModelCatalogTransportFuture,
    GrokModelCatalogTransportResponse, MAX_GROK_BILLING_BYTES, MAX_GROK_MODEL_CATALOG_BYTES,
    parse_grok_billing, parse_grok_model_catalog,
};
pub use compaction::{
    GrokCompactionDecodeError, GrokCompactionRequest, GrokCompactionSummaryDecoder,
};
pub use config::{
    GROK_CLI_BASE_URL, GROK_RESPONSES_URL, GrokProviderConfigError, GrokProviderTransport,
    XAI_PROVIDER_NAME,
};
pub use headers::{GrokClientIdentity, GrokHeader, GrokHeaderValue, build_grok_headers};
pub use inference::{
    GrokInferenceChunkStream, GrokInferenceClientCacheStatus, GrokInferenceDnsObservation,
    GrokInferenceDnsSource, GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
    GrokInferenceTransportMetrics,
};
pub use network::{
    GrokDnsResolutionError, GrokDnsResolutionPlan, GrokDnsResolutionPolicy, GrokEndpointPolicy,
    GrokReqwestTransportBuildError, OfficialGrokEndpointPolicy, ReqwestGrokInferenceTransport,
    ReqwestGrokModelCatalogTransport, ReqwestOAuthTransport,
};
pub(crate) use request::GrokReplayItem;
pub use request::{GrokRequestEncodeError, GrokResponsesRequest};
pub use session::{
    GrokCredentialFailure, GrokCredentialFeedbackFuture, GrokSessionAffinityKey,
    GrokSessionBinding, GrokSessionDataError, GrokSessionLeaseGuard, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture, SelectedGrokSession,
};

pub(crate) use upstream_error::{GrokQuotaFailureKind, classify_grok_quota_failure};
