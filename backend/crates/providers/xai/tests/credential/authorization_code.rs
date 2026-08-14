use provider_xai::{
    AuthorizationCallback, CallbackRejection, DiscoveryDocument, GrokOAuthConfig,
    PendingAuthorization, RedirectUriAllowlist,
};

const REDIRECT_URI: &str = "https://gateway.example/admin/xai/callback";

#[test]
fn callback_should_reject_duplicate_state() {
    let result = AuthorizationCallback::parse("code=fake&state=one&state=two");

    assert_eq!(
        result.expect_err("duplicate state must fail"),
        CallbackRejection::DuplicateParameter
    );
}

#[test]
fn callback_debug_should_redact_code_and_state() {
    let callback = AuthorizationCallback::parse("code=fake-code&state=fake-state")
        .expect("fixture callback is valid");

    let debug = format!("{callback:?}");

    assert!(!debug.contains("fake-code"), "debug output was {debug}");
}

#[test]
fn pending_authorization_should_round_trip_only_through_server_state() {
    let config = GrokOAuthConfig::official().expect("fixture config");
    let discovery = DiscoveryDocument::parse(&config, include_bytes!("fixtures/discovery.json"))
        .expect("fixture discovery");
    let allowed = RedirectUriAllowlist::new([REDIRECT_URI])
        .expect("redirect allowlist")
        .authorize(REDIRECT_URI)
        .expect("allowlisted redirect");
    let pending = PendingAuthorization::start(&config, &discovery, allowed, None)
        .expect("start pending flow");
    let authorization_url = pending.authorization_url().clone();
    let query = authorization_url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(query.get("plan").map(AsRef::as_ref), Some("generic"));
    assert_eq!(
        query.get("referrer").map(AsRef::as_ref),
        Some("codex-proxy-rs")
    );

    let state = pending.into_server_state().expect("serialize server state");
    let restored = PendingAuthorization::from_server_state(&config, &state)
        .expect("restore authenticated server state");
    assert_eq!(restored.authorization_url(), &authorization_url);

    let callback_state = authorization_url
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("authorization state");
    let callback =
        AuthorizationCallback::parse(&format!("code=one-time-code&state={callback_state}"))
            .expect("callback");
    restored
        .accept_callback(callback)
        .expect("restored state should validate callback");
}

#[test]
fn authorization_input_should_accept_callback_query_and_bare_code() {
    let (pending, state) = pending_authorization();
    pending
        .accept_authorization_input(&format!("{REDIRECT_URI}?code=full-url-code&state={state}"))
        .expect("official callback URL");

    let (pending, state) = pending_authorization();
    pending
        .accept_authorization_input(&format!("?code=query-code&state={state}"))
        .expect("callback query");

    let (pending, state) = pending_authorization();
    pending
        .accept_authorization_input(&format!("code=query-code&state={state}"))
        .expect("callback query without question mark");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input("  bare-code  ")
        .expect("bare authorization code");

    for code in [
        "opaque&state=still-code",
        "opaque&code=still-code",
        "opaque-error=value",
        "opaque://still-code",
    ] {
        let (pending, _) = pending_authorization();
        pending
            .accept_authorization_input(code)
            .expect("opaque bare authorization code");
    }
}

#[test]
fn authorization_input_should_keep_url_state_and_code_validation_strict() {
    let (pending, state) = pending_authorization();
    pending
        .accept_authorization_input(&format!(
            "https://attacker.invalid/callback?code=code&state={state}"
        ))
        .expect_err("foreign callback host");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input(&format!("{REDIRECT_URI}?code=code&state=wrong-state"))
        .expect_err("callback state mismatch");

    let (pending, state) = pending_authorization();
    pending
        .accept_authorization_input(&format!("{REDIRECT_URI}?code=code&state={state}#fragment"))
        .expect_err("callback fragment");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input("https:// malformed callback")
        .expect_err("malformed callback URL");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input("//attacker.invalid/callback?code=code")
        .expect_err("scheme-relative callback URL");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input("?unrelated=value")
        .expect_err("query input without code or state");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input("code=query-code")
        .expect_err("query input without state");

    let (pending, state) = pending_authorization();
    pending
        .accept_authorization_input(&format!("code=one&code=two&state={state}"))
        .expect_err("duplicate code");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input("code-with\n-control")
        .expect_err("control character");

    let (pending, state) = pending_authorization();
    pending
        .accept_authorization_input(&format!("?code=line%0Abreak&state={state}"))
        .expect_err("percent-decoded control character");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input(&"x".repeat(64 * 1024))
        .expect("maximum-size code");

    let (pending, _) = pending_authorization();
    pending
        .accept_authorization_input(&"x".repeat(64 * 1024 + 1))
        .expect_err("oversized code");
}

fn pending_authorization() -> (PendingAuthorization, String) {
    let config = GrokOAuthConfig::official().expect("fixture config");
    let discovery = DiscoveryDocument::parse(&config, include_bytes!("fixtures/discovery.json"))
        .expect("fixture discovery");
    let redirect = RedirectUriAllowlist::new([REDIRECT_URI])
        .expect("redirect allowlist")
        .authorize(REDIRECT_URI)
        .expect("allowlisted redirect");
    let pending = PendingAuthorization::start(&config, &discovery, redirect, None)
        .expect("start pending flow");
    let state = pending
        .authorization_url()
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("authorization state");
    (pending, state)
}
