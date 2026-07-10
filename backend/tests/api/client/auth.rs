use axum::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};
use codex_proxy_rs::api::client::auth::{bearer_client_api_key, ClientApiKeyAuthError};

#[test]
fn bearer_client_api_key_should_report_missing_authorization() {
    let headers = HeaderMap::new();

    let result = bearer_client_api_key(&headers);

    assert_eq!(result, Err(ClientApiKeyAuthError::MissingAuthorization));
}

#[test]
fn bearer_client_api_key_should_report_malformed_authorization() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Token sk_test"));

    let result = bearer_client_api_key(&headers);

    assert_eq!(result, Err(ClientApiKeyAuthError::MalformedAuthorization));
}

#[test]
fn bearer_client_api_key_should_report_empty_bearer_token() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer   "));

    let result = bearer_client_api_key(&headers);

    assert_eq!(result, Err(ClientApiKeyAuthError::MalformedAuthorization));
}

#[test]
fn bearer_client_api_key_should_report_invalid_key_format() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer admin-secret"),
    );

    let result = bearer_client_api_key(&headers);

    assert_eq!(result, Err(ClientApiKeyAuthError::InvalidKeyFormat));
}

#[test]
fn bearer_client_api_key_should_extract_trimmed_client_key() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer sk_test   "));

    let result = bearer_client_api_key(&headers);

    assert_eq!(result, Ok("sk_test"));
}
