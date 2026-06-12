use codex_proxy_rs::{
    codex::fingerprint::{model::Fingerprint, updater::parse_update_manifest},
    codex::transport::headers::build_codex_headers,
};

#[test]
fn codex_headers_include_desktop_identity_and_turn_state() {
    let fp = Fingerprint::default_for_tests();
    let headers = build_codex_headers(
        &fp,
        "access-token",
        Some("acct_123"),
        Some("turn-state"),
        "rid_1",
    );

    assert_eq!(headers.get("originator").unwrap(), "Codex Desktop");
    assert!(headers.get("user-agent").unwrap().contains("Codex"));
    assert_eq!(headers.get("authorization").unwrap(), "Bearer access-token");
    assert_eq!(headers.get("chatgpt-account-id").unwrap(), "acct_123");
    assert_eq!(headers.get("x-codex-turn-state").unwrap(), "turn-state");
    assert_eq!(headers.get("x-client-request-id").unwrap(), "rid_1");
}

#[test]
fn update_manifest_updates_app_version_and_build_number() {
    let manifest = r#"{"version":"26.600.12345","build_number":"4001"}"#;
    let update = parse_update_manifest(manifest).unwrap();
    assert_eq!(update.app_version, "26.600.12345");
    assert_eq!(update.build_number, "4001");
}
