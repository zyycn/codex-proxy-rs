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
    let request = provider_xai::GrokResponsesRequest::encode(
        &gateway_core::operation::GenerateRequest::new(vec![
            gateway_core::operation::Message::new(
                gateway_core::operation::MessageRole::User,
                vec![gateway_core::operation::ContentPart::Text(
                    "hello".to_owned(),
                )],
            )
            .expect("message"),
        ])
        .expect("request"),
        "grok-code-test",
    )
    .expect("encoded request");
    let headers = build_grok_headers(
        &instance(),
        &request,
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
