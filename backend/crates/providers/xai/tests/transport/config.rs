use gateway_core::routing::{InstanceHealth, ProviderInstance, ProviderInstanceId, ProviderKind};

use provider_xai::{GROK_CLI_BASE_URL, GrokProviderConfigError, GrokProviderInstanceConfig};

fn instance(base_url: &str) -> ProviderInstance {
    ProviderInstance::new(
        ProviderInstanceId::new("inst_grok").expect("valid instance"),
        ProviderKind::new("xai").expect("valid provider"),
        base_url.to_owned(),
        true,
        InstanceHealth::Healthy,
    )
}

#[test]
fn official_instance_should_compile_to_responses_endpoint() {
    let config = GrokProviderInstanceConfig::from_snapshot(&instance(GROK_CLI_BASE_URL))
        .expect("official endpoint is valid");

    assert_eq!(
        config.responses_url().as_str(),
        "https://cli-chat-proxy.grok.com/v1/responses"
    );
}

#[test]
fn instance_should_reject_cross_origin_endpoint() {
    let error = GrokProviderInstanceConfig::from_snapshot(&instance("https://api.x.ai/v1"))
        .expect_err("public API fallback must fail");

    assert_eq!(error, GrokProviderConfigError::UnsafeBaseUrl);
}
