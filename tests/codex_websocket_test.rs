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

fn base_request() -> CodexResponsesRequest {
    CodexResponsesRequest {
        model: "gpt-5.5".to_string(),
        instructions: String::new(),
        input: Vec::new(),
        stream: true,
        store: false,
        reasoning: None,
        tools: None,
        previous_response_id: None,
        use_websocket: false,
    }
}
