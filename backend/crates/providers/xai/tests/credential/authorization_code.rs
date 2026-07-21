use provider_xai::{
    AuthorizationCallback, CallbackRejection, DiscoveryDocument, GrokOAuthConfig,
    PendingAuthorization, RedirectUriAllowlist,
};

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
    let redirect = "https://gateway.example/admin/xai/callback";
    let allowed = RedirectUriAllowlist::new([redirect])
        .expect("redirect allowlist")
        .authorize(redirect)
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
