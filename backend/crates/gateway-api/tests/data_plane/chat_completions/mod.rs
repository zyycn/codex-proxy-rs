//! Chat 原始入口与交付生命周期测试，纯转换测试归 gateway-protocol

mod delivery;
mod http;

use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta};
use serde_json::{Value, json};

fn chat_request(stream: bool) -> Value {
    json!({"model":"model-a","messages":[{"role":"user","content":"hello"}],"stream":stream})
}

fn chunk(delta: Value, finish: Value) -> ProviderEvent {
    ProviderEvent::wire(
        ProtocolWireEvent::json(
            "chat_completions",
            None,
            json!({
                "id":"chatcmpl_fixture", "object":"chat.completion.chunk", "created":100,
                "model":"model-a", "choices":[{"index":0,"delta":delta,"finish_reason":finish}]
            }),
        )
        .unwrap(),
    )
}

fn completed(text: &str) -> ProviderEvent {
    chunk(json!({"content":text}), json!("stop")).with_fact(GatewayEvent::Completed(
        ResponseMeta::new("upstream_response", "model-a"),
    ))
}

fn created() -> ProviderEvent {
    chunk(json!({"role":"assistant"}), Value::Null)
}

fn json_completed() -> ProviderEvent {
    ProviderEvent::wire(ProtocolWireEvent::raw_json("chat_completions", bytes::Bytes::from(json!({
        "id":"chatcmpl_fixture", "object":"chat.completion", "created":100,
        "model":"model-a", "choices":[{"index":0,"message":{"role":"assistant","content":"answer"},"finish_reason":"stop"}]
    }).to_string())).unwrap()).with_fact(GatewayEvent::Completed(ResponseMeta::new("upstream_response", "model-a")))
}
