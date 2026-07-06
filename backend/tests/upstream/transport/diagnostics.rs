use codex_proxy_rs::upstream::transport::CodexUpstreamDiagnostics;
use reqwest::header::{HeaderMap, HeaderValue};

#[test]
fn diagnostics_should_extract_request_id_and_trace_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", HeaderValue::from_static("req_1"));
    headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(429), &headers);

    assert_eq!(diagnostics.status_code, Some(429));
    assert_eq!(diagnostics.request_id.as_deref(), Some("req_1"));
    assert_eq!(diagnostics.cf_ray(), Some("ray_1"));
}

#[test]
fn diagnostics_should_not_treat_cf_ray_as_request_id() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

    assert_eq!(diagnostics.request_id, None);
    assert_eq!(diagnostics.cf_ray(), Some("ray_1"));
}
