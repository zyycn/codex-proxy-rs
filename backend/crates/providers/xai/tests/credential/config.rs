use provider_xai::{ConfigError, GrokOAuthConfig, RedirectUriAllowlist};

#[test]
fn redirect_allowlist_should_accept_exact_https_callback() {
    let allowlist = RedirectUriAllowlist::new(["https://gateway.example/oauth/callback"])
        .expect("fixture allowlist is valid");

    let callback = allowlist
        .authorize("https://gateway.example/oauth/callback")
        .expect("exact callback is allowlisted");

    assert_eq!(
        callback.as_url().as_str(),
        "https://gateway.example/oauth/callback"
    );
}

#[test]
fn redirect_allowlist_should_reject_non_loopback_http_callback() {
    let result = RedirectUriAllowlist::new(["http://gateway.example/oauth/callback"]);

    assert_eq!(
        result.expect_err("HTTP callback must fail"),
        ConfigError::InvalidRedirectUri
    );
}

#[test]
fn discovered_endpoint_should_reject_cross_origin_url() {
    let config = GrokOAuthConfig::official().expect("fixture config is valid");

    let result = config.validate_discovered_endpoint("https://attacker.example/token");

    assert_eq!(
        result.expect_err("cross-origin endpoint must fail"),
        ConfigError::UntrustedEndpoint
    );
}

#[test]
fn discovered_issuer_should_reject_query_components() {
    let config = GrokOAuthConfig::official().expect("fixture config is valid");

    let result = config.validate_discovered_issuer("https://auth.x.ai?redirect=attacker");

    assert_eq!(
        result.expect_err("issuer query must fail"),
        ConfigError::UntrustedIssuer
    );
}
