use provider_xai::GROK_CLI_BASE_URL;
use provider_xai::transport::GROK_RESPONSES_URL;

#[test]
fn official_grok_build_endpoints_are_fixed_to_the_cli_origin() {
    assert_eq!(GROK_CLI_BASE_URL, "https://cli-chat-proxy.grok.com/v1");
    assert_eq!(
        GROK_RESPONSES_URL,
        "https://cli-chat-proxy.grok.com/v1/responses"
    );
}
