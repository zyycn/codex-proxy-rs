//! Grok Build CLI Responses provider boundary.

pub(crate) mod canonical;
pub(crate) mod catalog;
pub(crate) mod config;
pub(crate) mod headers;
pub(crate) mod network;
pub(crate) mod profile;
mod request;
mod session;
#[expect(
    clippy::module_inception,
    reason = "冻结架构要求 transport/transport.rs"
)]
mod transport;

pub use canonical::{GrokCanonicalDecoder, grok_billing_breakdown};
pub use catalog::{
    GROK_BILLING_URL, GROK_MODEL_CATALOG_URL, GrokBillingClient, GrokBillingError,
    GrokBillingRequest, GrokBillingSnapshot, GrokBillingTransport, GrokBillingTransportError,
    GrokBillingTransportErrorKind, GrokBillingTransportFuture, GrokBillingTransportResponse,
    GrokCatalogApiBackend, GrokCatalogCapabilities, GrokCatalogCapabilityEvidence,
    GrokCatalogLimits, GrokCatalogMetadata, GrokCatalogModel, GrokModelCatalogClient,
    GrokModelCatalogError, GrokModelCatalogRequest, GrokModelCatalogSession,
    GrokModelCatalogSessionError, GrokModelCatalogSnapshot, GrokModelCatalogTransport,
    GrokModelCatalogTransportError, GrokModelCatalogTransportErrorKind,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, MAX_GROK_BILLING_BYTES,
    MAX_GROK_MODEL_CATALOG_BYTES, parse_grok_billing, parse_grok_model_catalog,
};
pub use config::{
    GROK_CLI_BASE_URL, GrokProviderConfigError, GrokProviderInstanceConfig, GrokProviderTransport,
    XAI_PROVIDER_NAME,
};
pub use headers::{GrokClientIdentity, GrokHeader, GrokHeaderValue, build_grok_headers};
pub use network::{
    GrokDnsResolutionError, GrokDnsResolutionPlan, GrokDnsResolutionPolicy, GrokEndpointPolicy,
    GrokReqwestTransportBuildError, OfficialGrokEndpointPolicy, ReqwestGrokInferenceTransport,
    ReqwestGrokModelCatalogTransport, ReqwestOAuthTransport,
};
pub use profile::XaiWireProfileState;
pub use request::{GrokRequestEncodeError, GrokResponsesRequest};
pub use session::{
    GrokCredentialFailure, GrokCredentialFeedbackFuture, GrokSessionAffinityKey,
    GrokSessionBinding, GrokSessionDataError, GrokSessionLeaseGuard, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture, SelectedGrokSession,
};
pub use transport::{
    GrokInferenceChunkStream, GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
};
