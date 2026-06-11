use codex_proxy_rs::{codex::headers::build_codex_headers, fingerprint::model::Fingerprint};

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
