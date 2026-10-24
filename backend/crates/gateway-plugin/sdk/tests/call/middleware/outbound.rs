//! 出站候选目标和可信归属的独立 wire 合同

use gateway_plugin_sdk::call::middleware::outbound::{CallHead, RequestHead, Target};
use serde_json::json;

#[test]
fn outbound_websocket_head_keeps_duplicate_header_bytes() {
    let head: RequestHead = serde_json::from_value(json!({
        "target":{"kind":"websocket","uri":"wss://example.test/"},
        "headers":[{"name":"x-value","value":[128]},{"name":"x-value","value":[255]}]
    }))
    .unwrap();
    assert!(matches!(head.target, Target::WebSocket { .. }));
    assert_eq!(
        head.headers
            .iter()
            .map(|header| header.value.clone())
            .collect::<Vec<_>>(),
        vec![vec![128], vec![255]]
    );
}

#[test]
fn outbound_return_cannot_replace_host_metadata() {
    let value = json!({"target":{"kind":"http","method":"POST","uri":"https://example.test/"},"headers":[],"metadata":{"account_id":"forged"}});
    assert!(serde_json::from_value::<RequestHead>(value).is_err());
    assert!(serde_json::from_value::<CallHead>(json!({"request":{},"metadata":{}})).is_err());
}
