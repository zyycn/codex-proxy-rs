use gateway_core::engine::ModelRequestId;
use gateway_core::engine::credential::{CredentialRevision, ProviderAccountId};
use gateway_core::routing::{
    InstanceHealth, ProviderInstance, ProviderInstanceId, ProviderKind, UpstreamModelId,
};

use provider_xai::{
    GrokProviderInstanceConfig, GrokSessionBinding, SecretValue, SelectedGrokSession,
    build_grok_headers,
};

fn selected_session() -> SelectedGrokSession {
    SelectedGrokSession::new(
        ProviderAccountId::new("acct_grok_test").expect("account ID"),
        CredentialRevision::new(1).expect("revision"),
        SecretValue::new("fixture-access-token".to_owned()),
        SecretValue::new("fixture-user-id".to_owned()),
        Some(SecretValue::new("fixture@example.test".to_owned())),
        GrokSessionBinding::new("egress-fixture").expect("binding"),
        (),
    )
    .expect("selected session")
}

fn instance() -> GrokProviderInstanceConfig {
    let instance = ProviderInstance::new(
        ProviderInstanceId::new("inst_grok").expect("instance"),
        ProviderKind::new("xai").expect("provider"),
        "https://cli-chat-proxy.grok.com/v1".to_owned(),
        true,
        InstanceHealth::Healthy,
    );
    GrokProviderInstanceConfig::from_snapshot(&instance).expect("compiled instance")
}

#[test]
fn header_debug_should_redact_oauth_and_identity_values() {
    let headers = build_grok_headers(
        &instance(),
        &selected_session(),
        &ModelRequestId::new("req_grok_test").expect("request ID"),
        &UpstreamModelId::new("grok-code-test").expect("model"),
    );

    let debug = format!("{headers:?}");

    for secret in [
        "fixture-access-token",
        "fixture-user-id",
        "fixture@example.test",
        "req_grok_test",
    ] {
        assert!(!debug.contains(secret), "debug output was {debug}");
    }
}

#[test]
fn headers_should_bind_identity_to_the_selected_oauth_account() {
    let headers = build_grok_headers(
        &instance(),
        &selected_session(),
        &ModelRequestId::new("req_grok_identity").expect("request ID"),
        &UpstreamModelId::new("grok-code-test").expect("model"),
    );
    let value = |name: &str| {
        headers
            .iter()
            .find(|header| header.name().eq_ignore_ascii_case(name))
            .map(|header| header.value().expose())
    };

    assert_eq!(value("x-userid"), Some("fixture-user-id"));
    assert_eq!(value("x-grok-user-id"), Some("fixture-user-id"));
    assert_eq!(value("x-email"), Some("fixture@example.test"));
    assert_eq!(value("x-grok-conv-id"), Some("req_grok_identity"));
    assert_eq!(value("x-grok-session-id"), Some("req_grok_identity"));
    assert_eq!(value("x-grok-agent-id"), Some("codex-proxy-rs"));
}
