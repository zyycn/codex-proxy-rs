//! Chat 聚合器只聚合当前协议且要求显式终态

use gateway_protocol::chat_completions::{ChatAccumulator, ChatError};
use serde_json::{Value, json};

fn frame(delta: Value, reason: Value) -> Value {
    json!({"id":"c","model":"m","created":1,"object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
}

#[test]
fn assembles_interleaved_tools_and_late_usage_without_changing_identity() {
    let mut accumulator = ChatAccumulator::default();
    for value in [
        frame(
            json!({"role":"assistant","content":"hello ","tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"read","arguments":"{\"q\":"}},{"index":1,"id":"b","type":"custom","custom":{"name":"shell","input":"echo"}}]}),
            Value::Null,
        ),
        frame(
            json!({"content":"world","reasoning_content":"thought","tool_calls":[{"index":1,"custom":{"input":" ok"}},{"index":0,"function":{"arguments":"1}"}}]}),
            Value::Null,
        ),
        frame(json!({}), json!("tool_calls")),
        json!({"id":"c","model":"m","created":1,"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":5,"total_tokens":9}}),
    ] {
        accumulator.push(&value).unwrap();
    }
    let result = accumulator.finish().unwrap();
    assert_eq!(result["object"], "chat.completion");
    assert_eq!(result["choices"][0]["message"]["content"], "hello world");
    assert_eq!(
        result["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"q\":1}"
    );
    assert_eq!(
        result["choices"][0]["message"]["tool_calls"][1]["custom"]["input"],
        "echo ok"
    );
    assert_eq!(result["usage"]["total_tokens"], 9);
}

#[test]
fn truncation_and_post_terminal_content_are_rejected() {
    let mut accumulator = ChatAccumulator::default();
    accumulator
        .push(&frame(json!({"content":"partial"}), Value::Null))
        .unwrap();
    assert_eq!(
        accumulator.finish().unwrap_err(),
        ChatError::IncompleteStream
    );
    let mut accumulator = ChatAccumulator::default();
    accumulator.push(&frame(json!({}), json!("stop"))).unwrap();
    assert_eq!(
        accumulator
            .push(&frame(json!({"content":"extra"}), Value::Null))
            .unwrap_err(),
        ChatError::InvalidResponse
    );
}

#[test]
fn error_envelope_retains_recovery_fields_without_success() {
    let error = json!({"error":{"type":"overloaded_error","message":"busy","retry_after":3}});
    let mut accumulator = ChatAccumulator::default();
    accumulator.push(&error).unwrap();
    assert_eq!(accumulator.finish().unwrap(), error);
}

#[test]
fn tool_only_completions_keep_null_assistant_content() {
    let mut accumulator = ChatAccumulator::default();
    accumulator
        .push(&frame(
            json!({"role":"assistant","content":""}),
            Value::Null,
        ))
        .unwrap();
    accumulator.push(&frame(json!({"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"read","arguments":"{}"}}]}),json!("tool_calls"))).unwrap();
    assert!(accumulator.finish().unwrap()["choices"][0]["message"]["content"].is_null());
}

#[test]
fn complete_json_becomes_ordered_chunks_without_losing_tools_logprobs_or_usage() {
    let completion = json!({"id":"c","model":"m","created":1,"object":"chat.completion","service_tier":"priority","choices":[{"index":0,"message":{"role":"assistant","content":"answer","refusal":null,"reasoning_content":"thought","annotations":[{"type":"url_citation","url_citation":{"url":"https://example.org","start_index":0,"end_index":6}}],"tool_calls":[{"id":"call","type":"function","function":{"name":"read","arguments":"{}"}},{"id":"custom","type":"custom","custom":{"name":"shell","input":"echo ok"}}]},"finish_reason":"tool_calls","logprobs":{"content":[{"token":"answer","logprob":-0.1}]}}],"usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}});
    for include_usage in [false, true] {
        let chunks =
            gateway_protocol::chat_completions::completion_chunks(&completion, include_usage)
                .unwrap();
        assert_eq!(
            chunks[0]["choices"][0]["delta"],
            json!({"role":"assistant"})
        );
        assert_eq!(
            chunks[1]["choices"][0]["delta"]["tool_calls"][1]["index"],
            1
        );
        assert_eq!(chunks[2]["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(chunks.len(), if include_usage { 4 } else { 3 });
        if !include_usage {
            assert!(chunks.iter().all(|chunk| chunk.get("usage").is_none()));
        }
        let mut accumulator = ChatAccumulator::default();
        for chunk in &chunks {
            accumulator.push(chunk).unwrap();
        }
        let reconstructed = accumulator.finish().unwrap();
        assert_eq!(reconstructed["choices"], completion["choices"]);
        assert_eq!(reconstructed["service_tier"], "priority");
        if include_usage {
            assert_eq!(reconstructed["usage"], completion["usage"]);
        }
    }
}

#[test]
fn complete_json_requires_a_complete_chat_identity_and_terminal() {
    for completion in [
        json!({"object":"response"}),
        json!({"id":"c","model":"m","created":1,"object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"partial"},"finish_reason":null}]}),
    ] {
        assert_eq!(
            gateway_protocol::chat_completions::completion_chunks(&completion, true).unwrap_err(),
            ChatError::InvalidResponse
        );
    }
}

#[test]
fn buffered_images_replace_previews_without_losing_other_images() {
    let mut accumulator = ChatAccumulator::default();
    for (index, url) in [
        (0, "preview-a"),
        (1, "preview-b"),
        (0, "final-a"),
        (1, "final-b"),
    ] {
        accumulator
            .push(&frame(
                json!({"images":[{"index":index,"type":"image_url","image_url":{"url":url}}]}),
                Value::Null,
            ))
            .unwrap();
    }
    accumulator.push(&frame(json!({}), json!("stop"))).unwrap();
    let result = accumulator.finish().unwrap();
    let images = result["choices"][0]["message"]["images"]
        .as_array()
        .unwrap();
    assert_eq!(images.len(), 2);
    assert_eq!(images[0]["image_url"]["url"], "final-a");
    assert_eq!(images[1]["image_url"]["url"], "final-b");
    assert!(images.iter().all(|image| image.get("index").is_none()));
}
