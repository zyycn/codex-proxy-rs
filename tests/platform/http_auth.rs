use axum::http::{HeaderMap, HeaderValue};

use codex_proxy_rs::platform::http::auth::{admin_session_id, client_api_key};

#[test]
fn client_api_key_should_accept_only_cpr_bearer_tokens() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer cpr_local_client_key"),
    );
    assert_eq!(
        client_api_key(&headers).unwrap().as_str(),
        "cpr_local_client_key"
    );

    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer sk-official-openai-key"),
    );
    assert!(client_api_key(&headers).is_none());
}

#[test]
fn admin_session_id_should_ignore_client_api_key_and_extract_only_cookie() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer cpr_local_client_key"),
    );
    assert!(admin_session_id(&headers).is_none());

    headers.insert(
        "cookie",
        HeaderValue::from_static("theme=dark; cpr_admin_session=sess_123; other=1"),
    );
    assert_eq!(admin_session_id(&headers), Some("sess_123"));
}
