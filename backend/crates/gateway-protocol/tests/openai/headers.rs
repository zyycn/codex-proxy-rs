//! OpenAI 响应的私有身份与可公开诊断头分类

use gateway_protocol::openai::is_private_response_header;

#[test]
fn private_account_headers_should_be_case_insensitive_and_keep_public_diagnostics() {
    for name in [
        "chatgpt-account-id",
        " X-OpenAI-Project ",
        "x-codex-turn-metadata",
    ] {
        assert!(is_private_response_header(name), "exposed {name}");
    }
    for name in [
        "x-request-id",
        "x-ratelimit-remaining-requests",
        "x-provider-extension",
    ] {
        assert!(!is_private_response_header(name), "hidden {name}");
    }
}
