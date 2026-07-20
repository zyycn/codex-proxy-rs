use gateway_core::routing::{InstanceHealth, ProviderInstance, ProviderInstanceId, ProviderKind};
use provider_openai::{
    CodexProviderConfigError, CodexProviderInstanceConfig, CodexProviderTransport,
    OFFICIAL_CODEX_BASE_PATH, OfficialCodexOriginPolicy,
};

use crate::support::LoopbackCodexOriginPolicy;

fn instance(provider: &str, base_url: &str) -> ProviderInstance {
    ProviderInstance::new(
        ProviderInstanceId::new("inst_openai").expect("instance id"),
        ProviderKind::new(provider).expect("provider"),
        base_url.to_owned(),
        true,
        InstanceHealth::Healthy,
    )
}

#[test]
fn official_instance_requires_exact_https_origin_and_base_path() {
    let compiled = CodexProviderInstanceConfig::from_snapshot(
        &instance("openai", "https://chatgpt.com/backend-api/"),
        &OfficialCodexOriginPolicy,
    )
    .expect("official endpoint");
    assert_eq!(compiled.base_url().path(), OFFICIAL_CODEX_BASE_PATH);
    assert_eq!(
        compiled.transport(),
        CodexProviderTransport::PreferWebSocket
    );
}

#[test]
fn official_policy_rejects_lookalike_private_and_metadata_origins() {
    for endpoint in [
        "https://api.chatgpt.com/backend-api",
        "https://chatgpt.com.evil.invalid/backend-api",
        "https://chatgpt.com:8443/backend-api",
        "https://chatgpt.com/other",
        "https://169.254.169.254/backend-api",
        "https://10.0.0.1/backend-api",
        "http://chatgpt.com/backend-api",
    ] {
        assert_eq!(
            CodexProviderInstanceConfig::from_snapshot(
                &instance("openai", endpoint),
                &OfficialCodexOriginPolicy,
            ),
            Err(CodexProviderConfigError::UnsafeBaseUrl),
            "endpoint must fail closed: {endpoint}"
        );
    }
}

#[test]
fn loopback_policy_requires_explicit_numeric_loopback_host() {
    let numeric = instance("openai", "http://127.0.0.1:43123/backend-api");
    assert_eq!(
        CodexProviderInstanceConfig::from_snapshot(&numeric, &OfficialCodexOriginPolicy),
        Err(CodexProviderConfigError::UnsafeBaseUrl)
    );
    assert!(
        CodexProviderInstanceConfig::from_snapshot(&numeric, &LoopbackCodexOriginPolicy).is_ok()
    );
    assert_eq!(
        CodexProviderInstanceConfig::from_snapshot(
            &instance("openai", "http://localhost:43123/backend-api"),
            &LoopbackCodexOriginPolicy,
        ),
        Err(CodexProviderConfigError::UnsafeBaseUrl)
    );
}

#[test]
fn non_codex_instance_is_rejected_before_transport_initialization() {
    assert_eq!(
        CodexProviderInstanceConfig::from_snapshot(
            &instance("xai", "https://chatgpt.com/backend-api"),
            &OfficialCodexOriginPolicy,
        ),
        Err(CodexProviderConfigError::ProviderMismatch)
    );
}
