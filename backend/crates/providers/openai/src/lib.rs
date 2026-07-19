//! Codex Provider 专属能力。

mod provider;

pub use provider::{
    CodexEndpointPolicy, CodexProvider, CodexProviderConfigError, CodexProviderInstanceConfig,
    CodexProviderTransport, OFFICIAL_CODEX_BASE_PATH,
};

pub mod credential;
pub mod transport;

pub use transport::{
    CodexCanonicalDecoder, CodexRequestEncodeError, codex_request_semantics,
    encode_generate_request, openai_billing_breakdown,
};
