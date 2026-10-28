//! Chat Completions 的请求转换、输出投影与 HTTP 生命周期测试

mod http;
mod request;
mod response;

use gateway_core::event::{ProtocolWireEvent, ProviderEvent};
use serde_json::{Value, json};

fn chat_request(stream: bool) -> Value {
    json!({"model":"model-a","messages":[{"role":"user","content":"hello"}],"stream":stream})
}

fn wire(kind: &str, mut value: Value) -> ProviderEvent {
    value["type"] = json!(kind);
    ProviderEvent::wire(ProtocolWireEvent::json("openai", Some(kind.to_owned()), value).unwrap())
}

fn completed(output: Value) -> ProviderEvent {
    wire(
        "response.completed",
        json!({"response":{
            "id":"resp_chat_test","object":"response","model":"upstream-model",
            "status":"completed","output":output,
            "usage":{"input_tokens":11,"output_tokens":7,"total_tokens":18,
                "input_tokens_details":{"cached_tokens":3},
                "output_tokens_details":{"reasoning_tokens":2}}
        }}),
    )
}

fn message(text: &str) -> Value {
    json!({"id":"msg_chat_test","type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":text,"annotations":[]}]})
}

fn created() -> ProviderEvent {
    wire(
        "response.created",
        json!({"response":{
            "id":"resp_chat_test","object":"response","model":"upstream-model",
            "status":"in_progress","output":[]
        }}),
    )
}
