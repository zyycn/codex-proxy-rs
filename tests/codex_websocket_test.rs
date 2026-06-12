use codex_proxy_rs::codex::{
    types::CodexResponsesRequest,
    websocket::{ensure_http_sse_supported, transport_for_request, CodexTransport},
};

#[test]
fn transport_for_request_should_allow_http_sse_without_websocket_only_fields() {
    let request = base_request();

    assert_eq!(transport_for_request(&request), CodexTransport::HttpSse);
    assert!(ensure_http_sse_supported(&request).is_ok());
}

#[test]
fn transport_for_request_should_require_websocket_for_previous_response_id() {
    let mut request = base_request();
    request.previous_response_id = Some("resp_123".to_string());

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketRequired
    );
    assert_eq!(
        ensure_http_sse_supported(&request).unwrap_err().to_string(),
        "previous_response_id requires Codex WebSocket transport"
    );
}

#[test]
fn use_websocket_should_not_serialize_to_upstream_json() {
    let mut request = base_request();
    request.use_websocket = true;

    let body = serde_json::to_value(&request).unwrap();

    assert!(body.get("use_websocket").is_none());
    assert!(body.get("useWebSocket").is_none());
}

fn base_request() -> CodexResponsesRequest {
    CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new())
}
