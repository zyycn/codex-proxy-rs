use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

use gateway_api::openai::auth::{
    ClientApiKeyAuthError, bearer_client_api_key, identify_codex_client,
};
use gateway_core::policy::CodexClientKind;

#[test]
fn bearer_client_api_key_should_reject_missing_authorization() {
    assert_eq!(
        bearer_client_api_key(&HeaderMap::new()),
        Err(ClientApiKeyAuthError::MissingAuthorization)
    );
}

#[test]
fn actor_authorization_marker_should_not_replace_client_api_key() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-openai-actor-authorization",
        HeaderValue::from_static("proxy-managed"),
    );

    assert_eq!(
        bearer_client_api_key(&headers),
        Err(ClientApiKeyAuthError::MissingAuthorization)
    );
}

#[test]
fn actor_authorization_marker_should_preserve_existing_bearer_authentication() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-openai-actor-authorization",
        HeaderValue::from_static("proxy-managed"),
    );
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer sk_client"));

    assert_eq!(bearer_client_api_key(&headers), Ok("sk_client"));
}

#[test]
fn bearer_client_api_key_should_reject_non_utf8_authorization() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_bytes(&[0xff]).expect("opaque header value"),
    );

    assert_eq!(
        bearer_client_api_key(&headers),
        Err(ClientApiKeyAuthError::MalformedAuthorization)
    );
}

#[test]
fn bearer_client_api_key_should_require_exact_bearer_scheme() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("bearer sk_client"));

    assert_eq!(
        bearer_client_api_key(&headers),
        Err(ClientApiKeyAuthError::MalformedAuthorization)
    );
}

#[test]
fn bearer_client_api_key_should_reject_empty_bearer_token() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer    "));

    assert_eq!(
        bearer_client_api_key(&headers),
        Err(ClientApiKeyAuthError::MalformedAuthorization)
    );
}

#[test]
fn bearer_client_api_key_should_accept_migrated_formats_without_rewriting() {
    for key in [
        "q".to_owned(),
        "sk-old/key+value=:!@".to_owned(),
        "x".repeat(8192),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}")).unwrap(),
        );
        assert_eq!(bearer_client_api_key(&headers), Ok(key.as_str()));
    }
}

#[test]
fn bearer_client_api_key_should_reject_whitespace_inside_tokens() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer key with spaces"),
    );
    assert_eq!(
        bearer_client_api_key(&headers),
        Err(ClientApiKeyAuthError::InvalidKeyFormat)
    );
}

#[test]
fn bearer_client_api_key_should_return_trimmed_gateway_key() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer   sk_client_secret   "),
    );

    assert_eq!(bearer_client_api_key(&headers), Ok("sk_client_secret"));
}

#[test]
fn client_auth_failure_reasons_should_be_stable_and_secret_free() {
    assert_eq!(
        [
            ClientApiKeyAuthError::MissingAuthorization,
            ClientApiKeyAuthError::MalformedAuthorization,
            ClientApiKeyAuthError::InvalidKeyFormat,
            ClientApiKeyAuthError::InvalidKey,
            ClientApiKeyAuthError::RuntimeUnavailable,
        ]
        .map(ClientApiKeyAuthError::reason),
        [
            "missing_authorization",
            "malformed_authorization",
            "invalid_key_format",
            "invalid_key",
            "runtime_unavailable",
        ]
    );
}

#[test]
fn desktop_headers_should_take_precedence_over_embedded_cli_marker() {
    let mut headers = HeaderMap::new();
    headers.insert("originator", HeaderValue::from_static("Codex Desktop"));
    headers.insert("version", HeaderValue::from_static("26.825.6671"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static("codex_cli_rs/0.39.0 terminal (Codex Desktop; 26.1.0)"),
    );

    let identified = identify_codex_client(&headers).expect("recognized desktop");

    assert_eq!(identified.kind(), CodexClientKind::Desktop);
    assert_eq!(
        identified.version().map(ToString::to_string).as_deref(),
        Some("26.825.6671")
    );
}

#[test]
fn cli_user_agent_should_expose_semver_or_recognized_missing_version() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "user-agent",
        HeaderValue::from_static("codex_cli_rs/0.39.0 (Linux; x86_64)"),
    );
    let identified = identify_codex_client(&headers).expect("recognized CLI");
    assert_eq!(identified.kind(), CodexClientKind::Cli);
    assert_eq!(
        identified.version().map(ToString::to_string).as_deref(),
        Some("0.39.0")
    );

    headers.insert(
        "user-agent",
        HeaderValue::from_static("codex-cli/not-a-version"),
    );
    assert!(
        identify_codex_client(&headers)
            .expect("recognized invalid CLI")
            .version()
            .is_none()
    );
}

#[test]
fn chatgpt_remote_desktop_without_app_version_should_not_use_a_version_gate() {
    for name in [
        "codex_chatgpt_android_remote",
        "codex_chatgpt_ios_remote",
        "codex_chatgpt_future_os_remote",
    ] {
        for remote_version in ["dev", "1.2.3"] {
            for (product, originator) in [
                ("Codex Desktop", None),
                ("codex_cli_rs", Some("Codex Desktop")),
            ] {
                let mut headers = HeaderMap::new();
                headers.insert(
                    "user-agent",
                    HeaderValue::from_str(&format!(
                        "{product}/0.154.0-alpha.6.2 (Windows 10.0.26200; x86_64) unknown ({name}; {remote_version})"
                    ))
                    .unwrap(),
                );
                if let Some(originator) = originator {
                    headers.insert("originator", HeaderValue::from_static(originator));
                }
                assert_eq!(identify_codex_client(&headers), None, "{headers:?}");
            }
        }
    }
}

#[test]
fn chatgpt_remote_marker_should_require_a_complete_exact_client_suffix() {
    for suffix in [
        "(codex_chatgpt_android_remote_extra; dev)",
        "(unofficial_codex_chatgpt_ios_remote; dev)",
        "(codex_chatgpt__remote; dev)",
        "(codex_chatgpt_remote; dev)",
        "(codex_chatgpt_future os_remote; dev)",
        "(codex_chatgpt_future;os_remote; dev)",
        "codex_chatgpt_android_remote; dev",
        "(codex_chatgpt_android_remote; dev",
        "(codex_chatgpt_android_remote; dev) trailing",
        "(codex_chatgpt_android_remote; dev) (another_client; dev)",
        "(codex_chatgpt_android_remote; )",
        "(another_client; codex_chatgpt_android_remote)",
        "(Codex Desktop; dev)",
        "(Codex Desktop; ) (codex_chatgpt_android_remote; dev)",
        "(Codex Desktop; invalid) (codex_chatgpt_ios_remote; dev)",
        "(Codex Desktop)",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            "user-agent",
            HeaderValue::from_str(&format!("Codex Desktop/0.154.0-alpha.6.2 {suffix}")).unwrap(),
        );
        let client = identify_codex_client(&headers).expect("Desktop still requires a version");
        assert_eq!(client.kind(), CodexClientKind::Desktop, "{suffix}");
        assert!(client.version().is_none(), "{suffix}");
    }
}

#[test]
fn chatgpt_remote_desktop_should_preserve_explicit_version_validation() {
    for (value, expected) in [
        (
            HeaderValue::from_static("26.908.70816"),
            Some("26.908.70816"),
        ),
        (HeaderValue::from_static("26.1.0"), Some("26.1.0")),
        (HeaderValue::from_static("dev"), None),
        (HeaderValue::from_static(""), None),
        (HeaderValue::from_bytes(&[0xff]).unwrap(), None),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("version", value);
        headers.insert(
            "user-agent",
            HeaderValue::from_static(
                "Codex Desktop/0.154.0-alpha.6.2 (codex_chatgpt_android_remote; dev)",
            ),
        );
        let client = identify_codex_client(&headers).expect("explicit Desktop version");
        assert_eq!(client.kind(), CodexClientKind::Desktop);
        assert_eq!(
            client.version().map(ToString::to_string).as_deref(),
            expected
        );
    }
}

#[test]
fn chatgpt_remote_marker_should_not_override_an_existing_desktop_version_suffix() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "user-agent",
        HeaderValue::from_static(
            "Codex Desktop/0.154.0-alpha.6.2 (Codex Desktop; 26.1.0) (codex_chatgpt_ios_remote; dev)",
        ),
    );
    let client = identify_codex_client(&headers).expect("Desktop version remains available");
    assert_eq!(client.version().unwrap().to_string(), "26.1.0");
}

#[test]
fn chatgpt_remote_suffix_created_by_header_truncation_should_not_skip_the_gate() {
    let suffix = " (codex_chatgpt_android_remote; dev)";
    let mut user_agent = "Codex Desktop/0.154.0-alpha.6.2 ".to_owned();
    user_agent.push_str(&"x".repeat(4096 - user_agent.len() - suffix.len()));
    user_agent.push_str(suffix);
    user_agent.push_str(" (another_client; dev)");
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", HeaderValue::from_str(&user_agent).unwrap());

    let client =
        identify_codex_client(&headers).expect("incomplete headers still require a version");
    assert_eq!(client.kind(), CodexClientKind::Desktop);
    assert!(client.version().is_none());
}
