use base64::{Engine as _, engine::general_purpose::STANDARD};
use provider_openai::transport::CodexUpstreamDiagnostics;
use reqwest::header::{HeaderMap, HeaderValue};

#[test]
fn diagnostics_should_extract_request_id_and_trace_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", HeaderValue::from_static("req_1"));
    headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(429), &headers);

    assert_eq!(
        (
            diagnostics.status_code,
            diagnostics.request_id.as_deref(),
            diagnostics.cf_ray(),
        ),
        (Some(429), Some("req_1"), Some("ray_1"))
    );
}

#[test]
fn diagnostics_should_not_treat_cf_ray_as_a_request_id() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

    assert_eq!(
        (diagnostics.request_id.as_deref(), diagnostics.cf_ray()),
        (None, Some("ray_1"))
    );
}

#[test]
fn diagnostics_should_accept_the_oai_request_id_header() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oai-request-id", HeaderValue::from_static("oai_req_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(500), &headers);

    assert_eq!(diagnostics.request_id.as_deref(), Some("oai_req_1"));
}

#[test]
fn diagnostics_should_extract_only_the_identity_error_code() {
    let encoded = STANDARD.encode(r#"{"error":{"code":"token_expired","message":"secret"}}"#);
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-openai-authorization-error",
        HeaderValue::from_static("authorization failed"),
    );
    headers.insert(
        "x-error-json",
        HeaderValue::from_str(&encoded).expect("encoded header"),
    );

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

    assert_eq!(
        (
            diagnostics.identity_authorization_error.as_deref(),
            diagnostics.identity_error_code.as_deref(),
        ),
        (Some("authorization failed"), Some("token_expired"))
    );
    assert!(!format!("{diagnostics:?}").contains("secret"));
}

#[test]
fn diagnostics_should_ignore_malformed_identity_error_json() {
    let mut headers = HeaderMap::new();
    headers.insert("x-error-json", HeaderValue::from_static("not-base64"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

    assert_eq!(diagnostics.identity_error_code, None);
}
