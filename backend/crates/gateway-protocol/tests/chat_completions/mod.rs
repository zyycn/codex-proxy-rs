//! Chat 请求解析、聚合与直接转换合同

mod genai;
mod messages;
mod request;
mod response;
mod responses;

use gateway_protocol::chat_completions::ChatIdentity;
use serde_json::{Map, Value, json};

fn identity() -> ChatIdentity {
    ChatIdentity {
        id: "chatcmpl-test".into(),
        created: 10,
        model: "public-model".into(),
    }
}
fn request() -> Map<String, Value> {
    json!({"model":"public-model","messages":[{"role":"user","content":"hello"}]})
        .as_object()
        .unwrap()
        .clone()
}
