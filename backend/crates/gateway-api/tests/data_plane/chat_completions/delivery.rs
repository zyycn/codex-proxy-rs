//! Chat 字节交付只消费 Chat wire，并核对终态、身份与截断

use super::{
    chat_request, chunk, completed, created,
    http::{Fixture, json_body},
};
use crate::support::generation::Session;
use axum::{body::to_bytes, http::StatusCode, response::Response};
use serde_json::{Value, json};

pub(super) async fn sse_body(response: Response) -> (String, Vec<Value>) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(text.matches("data: [DONE]").count(), 1, "{text}");
    assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
    let chunks = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    (text, chunks)
}

#[tokio::test]
async fn buffers_chat_chunks_as_json() {
    let fixture = Fixture::new(Session::new(vec![
        created(),
        chunk(json!({"content":"你好 "}), Value::Null),
        completed("🌍"),
    ]))
    .await;
    let response = fixture.post(chat_request(false)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "你好 🌍");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
}

#[tokio::test]
async fn returns_chat_sse_with_one_terminal_and_one_done() {
    let fixture = Fixture::new(Session::new(vec![created(), completed("answer")])).await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    assert!(
        chunks
            .iter()
            .all(|value| value["object"] == "chat.completion.chunk")
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|value| value["choices"][0]["finish_reason"].is_string())
            .count(),
        1
    );
    assert!(chunks.iter().all(|value| value["id"] == "chatcmpl_fixture"));
}

#[tokio::test]
async fn truncation_produces_error_without_success_finish() {
    let fixture = Fixture::new(Session::new(vec![
        created(),
        chunk(json!({"content":"partial"}), Value::Null),
    ]))
    .await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    assert!(chunks.iter().any(|value| value.get("error").is_some()));
    assert!(
        !chunks
            .iter()
            .any(|value| value["choices"][0]["finish_reason"].is_string())
    );
}

#[tokio::test]
async fn rejects_invalid_chat_input_before_starting_execution() {
    let fixture = Fixture::new(Session::new(Vec::new())).await;
    let mut body = chat_request(false);
    body["n"] = json!(2);
    let response = fixture.post(body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await["error"]["param"], "n");
    assert!(fixture.execution.captured.lock().unwrap().is_none());
}

#[tokio::test]
async fn buffered_chat_rejects_multiple_json_or_mixed_representations() {
    for events in [
        vec![super::json_completed(), super::json_completed()],
        vec![created(), super::json_completed()],
        vec![super::json_completed(), created()],
    ] {
        let fixture = Fixture::new(Session::new(vec![]).with_batch(events)).await;
        let response = fixture.post(chat_request(false)).await;
        assert!(response.status().is_server_error());
        assert!(json_body(response).await.get("choices").is_none());
    }
}

#[tokio::test]
async fn final_usage_follows_the_finish_chunk_and_precedes_done() {
    use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta};
    let usage = ProviderEvent::wire(ProtocolWireEvent::json("chat_completions", None, json!({
        "id":"chatcmpl_fixture","object":"chat.completion.chunk","created":100,"model":"model-a",
        "choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}
    })).unwrap()).with_fact(GatewayEvent::Completed(ResponseMeta::new("upstream", "model-a")));
    let fixture = Fixture::new(Session::new(vec![
        created(),
        chunk(json!({"content":"answer"}), Value::Null),
        chunk(json!({}), json!("stop")),
        usage,
    ]))
    .await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    assert_eq!(
        chunks[chunks.len() - 2]["choices"][0]["finish_reason"],
        "stop"
    );
    assert_eq!(chunks.last().unwrap()["usage"]["total_tokens"], 5);
    assert!(chunks.iter().all(|value| value.get("error").is_none()));
}
