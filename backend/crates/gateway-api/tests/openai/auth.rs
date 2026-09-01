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
fn bearer_client_api_key_should_reject_non_gateway_key_prefix() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer xai-secret"));

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
