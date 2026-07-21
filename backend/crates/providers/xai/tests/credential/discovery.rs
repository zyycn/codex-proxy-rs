use provider_xai::{DiscoveryDocument, FailureClass, GrokOAuthConfig, OAuthError};

const VALID_DISCOVERY: &str = include_str!("fixtures/discovery.json");

#[test]
fn discovery_should_accept_official_origin_fixture() {
    let config = GrokOAuthConfig::official().expect("fixture config is valid");

    let document = DiscoveryDocument::parse(&config, VALID_DISCOVERY.as_bytes())
        .expect("official discovery fixture is valid");

    assert_eq!(document.token_endpoint().host_str(), Some("auth.x.ai"));
    assert_eq!(document.userinfo_endpoint().path(), "/oauth2/userinfo");
}

#[test]
fn discovery_should_reject_cross_origin_token_endpoint() {
    let config = GrokOAuthConfig::official().expect("fixture config is valid");
    let malicious = VALID_DISCOVERY.replace(
        "https://auth.x.ai/oauth2/token",
        "https://attacker.example/oauth2/token",
    );

    let error = DiscoveryDocument::parse(&config, malicious.as_bytes())
        .expect_err("cross-origin endpoint must fail");

    assert_eq!(error.class(), FailureClass::Security);
}

#[test]
fn discovery_should_reject_none_signing_algorithm() {
    let config = GrokOAuthConfig::official().expect("fixture config is valid");
    let insecure = VALID_DISCOVERY.replace("\"ES256\"", "\"none\"");

    let error = DiscoveryDocument::parse(&config, insecure.as_bytes())
        .expect_err("none algorithm must fail");

    assert!(matches!(error, OAuthError::Protocol { .. }));
}

#[test]
fn discovery_should_reject_missing_signing_algorithms() {
    let config = GrokOAuthConfig::official().expect("fixture config is valid");
    let incomplete = VALID_DISCOVERY.replace(
        "\"id_token_signing_alg_values_supported\": [\"ES256\"]",
        "\"id_token_signing_alg_values_supported\": []",
    );

    let error = DiscoveryDocument::parse(&config, incomplete.as_bytes())
        .expect_err("missing algorithms must fail");

    assert!(matches!(error, OAuthError::Protocol { .. }));
}

#[test]
fn discovery_should_require_same_origin_userinfo_endpoint() {
    let config = GrokOAuthConfig::official().expect("fixture config is valid");
    let malicious = VALID_DISCOVERY.replace(
        "https://auth.x.ai/oauth2/userinfo",
        "https://attacker.example/oauth2/userinfo",
    );

    let error = DiscoveryDocument::parse(&config, malicious.as_bytes())
        .expect_err("cross-origin userinfo endpoint must fail");

    assert_eq!(error.class(), FailureClass::Security);
}
