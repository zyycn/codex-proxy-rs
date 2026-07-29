//! Grok Build CLI Responses provider 边界。

pub(crate) mod canonical;
pub(crate) mod catalog;
pub(crate) mod compaction;
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
    GrokCatalogLimits, GrokCatalogMetadata, GrokCatalogModel, GrokCatalogReasoningEffort,
    GrokModelCatalogClient, GrokModelCatalogError, GrokModelCatalogRequest,
    GrokModelCatalogSession, GrokModelCatalogSessionError, GrokModelCatalogSnapshot,
    GrokModelCatalogTransport, GrokModelCatalogTransportError, GrokModelCatalogTransportErrorKind,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, MAX_GROK_BILLING_BYTES,
    MAX_GROK_MODEL_CATALOG_BYTES, parse_grok_billing, parse_grok_model_catalog,
};
pub use compaction::{
    GrokCompactionDecodeError, GrokCompactionRequest, GrokCompactionSummaryDecoder,
};
pub use config::{
    GROK_CLI_BASE_URL, GROK_RESPONSES_URL, GrokProviderConfigError, GrokProviderTransport,
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
    GrokInferenceChunkStream, GrokInferenceClientCacheStatus, GrokInferenceDnsObservation,
    GrokInferenceDnsSource, GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
    GrokInferenceTransportMetrics,
};

const UUID_TEXT_LEN: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrokQuotaFailureKind {
    Account,
    FreeAccount,
    Model,
}

pub(crate) fn classify_grok_quota_failure(
    code: Option<&str>,
    error_type: Option<&str>,
    message: Option<&str>,
) -> Option<GrokQuotaFailureKind> {
    let text = [
        code.unwrap_or_default(),
        error_type.unwrap_or_default(),
        message.unwrap_or_default(),
    ]
    .join(" ")
    .to_ascii_lowercase();
    let free_usage = contains_any(
        &text,
        &[
            "subscription:free-usage-exhausted",
            "subscription_free_usage_exhausted",
            "free-usage-exhausted",
            "free_usage_exhausted",
            "used all the included free usage",
            "used all your free usage",
        ],
    );
    if free_usage && text.contains("for model") {
        return Some(GrokQuotaFailureKind::Model);
    }
    if contains_any(
        &text,
        &[
            "personal-team-blocked:spending-limit",
            "personal_team_blocked_spending_limit",
            "quota_exceeded",
            "insufficient_quota",
        ],
    ) {
        return Some(GrokQuotaFailureKind::Account);
    }
    free_usage.then_some(GrokQuotaFailureKind::FreeAccount)
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

/// 把 message 中的 UUID 替换为占位符，控制字符归一为空格，
/// 使文本满足客户端可见错误的约束且不携带可定位账号的标识。
fn scrub_account_fingerprints(message: &str) -> String {
    let bytes = message.as_bytes();
    let mut scrubbed = String::with_capacity(message.len());
    let mut index = 0;
    while index < message.len() {
        if uuid_at(bytes, index) {
            scrubbed.push_str("[redacted]");
            index += UUID_TEXT_LEN;
            continue;
        }
        let character = message[index..].chars().next().expect("char boundary");
        scrubbed.push(if character.is_control() {
            ' '
        } else {
            character
        });
        index += character.len_utf8();
    }
    scrubbed
}

fn uuid_at(bytes: &[u8], index: usize) -> bool {
    let Some(candidate) = bytes.get(index..index + UUID_TEXT_LEN) else {
        return false;
    };
    if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
        return false;
    }
    if bytes
        .get(index + UUID_TEXT_LEN)
        .is_some_and(u8::is_ascii_alphanumeric)
    {
        return false;
    }
    candidate.iter().enumerate().all(|(offset, byte)| {
        if matches!(offset, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}
