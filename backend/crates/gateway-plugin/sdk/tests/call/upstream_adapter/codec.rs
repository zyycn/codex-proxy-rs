//! 验证上游请求的大正文往返与响应事件预算保持独立

use gateway_plugin_sdk::call::{
    model::{ExecutionEvent, MAX_EXECUTION_PAYLOAD_BYTES, WireEvent, WirePayload},
    upstream_adapter::UpstreamAdapterRequest,
};
use serde_json::json;

fn request() -> UpstreamAdapterRequest {
    serde_json::from_value(json!({
        "adapter_id":"example", "provider":"openai", "upstream_model":"fixture",
        "client_key_id":"key", "account_id":"account", "credential_revision":1,
        "protocol":"openai", "client_transport":"http_json", "fast_mode":"default",
        "headers":[["x-fixture",[97,98]]], "continuation":null,
        "request_elapsed_ms":0
    }))
    .unwrap()
}

#[test]
fn large_request_preserves_bytes_and_rejects_malformed_lengths() {
    let body = (0..MAX_EXECUTION_PAYLOAD_BYTES + 1)
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    let encoded = request().encode(body.clone()).unwrap();
    let (decoded, actual) = UpstreamAdapterRequest::decode(&encoded).unwrap();
    assert_eq!(actual, body);
    assert_eq!(decoded.headers, request().headers);
    assert!(UpstreamAdapterRequest::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut corrupted = encoded;
    corrupted[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(UpstreamAdapterRequest::decode(&corrupted).is_err());
}

#[test]
fn large_requests_do_not_expand_response_event_budget() {
    let event = ExecutionEvent {
        wire: Some(WireEvent {
            protocol: "openai".into(),
            payload: WirePayload::RawBody {
                body: vec![b'x'; MAX_EXECUTION_PAYLOAD_BYTES],
            },
        }),
        ..Default::default()
    };
    assert!(event.encode().is_err());
}
