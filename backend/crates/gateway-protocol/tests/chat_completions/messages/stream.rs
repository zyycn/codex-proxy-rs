//! Messages SSE 的块顺序、终态、工具与累计用量合同

use gateway_protocol::anthropic::message_to_stream_events;
use gateway_protocol::chat_completions::{
    ChatAccumulator, ChatError,
    messages::{ChatStreamEncoder, project_response_with_request},
};
use serde_json::{Value, json};

#[test]
fn native_json_and_stream_produce_the_same_chat_result() {
    let mut request = super::super::request();
    request.insert(
        "tools".into(),
        json!([{"type":"custom","custom":{"name":"shell"}}]),
    );
    let native = json!({"id":"native","type":"message","role":"assistant","content":[{"type":"text","text":"answer"},{"type":"thinking","thinking":"reason","signature":"private"},{"type":"tool_use","id":"a","name":"shell","input":{"input":"pwd"}}],"stop_reason":"tool_use","usage":{"input_tokens":3,"output_tokens":7}});
    let identity = super::super::identity();
    let expected = project_response_with_request(&native, &identity, &request).unwrap();
    let mut encoder = ChatStreamEncoder::with_request(identity, true, &request);
    let mut accumulator = ChatAccumulator::default();
    for event in message_to_stream_events(native).unwrap() {
        for chunk in encoder.push_event(None, &event).unwrap() {
            accumulator.push(&chunk).unwrap();
        }
    }
    assert!(encoder.is_completed());
    assert!(encoder.finish().unwrap().is_empty());
    assert_eq!(accumulator.finish().unwrap(), expected);
}

fn start() -> Value {
    json!({"type":"message_start","message":{"type":"message","role":"assistant","content":[],"usage":{"input_tokens":3,"output_tokens":0}}})
}

#[test]
fn streams_function_arguments_and_defers_custom_json_unwrapping_until_block_stop() {
    let mut request = super::super::request();
    request.insert(
        "tools".into(),
        json!([{"type":"custom","custom":{"name":"shell"}}]),
    );
    let mut encoder = ChatStreamEncoder::with_request(super::super::identity(), true, &request);
    let mut accumulator = ChatAccumulator::default();
    for event in [
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"a","name":"shell","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"echo \\"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"ok\\\"\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
        json!({"type":"message_stop"}),
    ] {
        for chunk in encoder.push_event(None, &event).unwrap() {
            accumulator.push(&chunk).unwrap();
        }
    }
    let result = accumulator.finish().unwrap();
    assert_eq!(
        result["choices"][0]["message"]["tool_calls"][0]["custom"]["input"],
        "echo \"ok\""
    );
    assert_eq!(result["usage"]["completion_tokens"], 9);
}

#[test]
fn truncation_duplicate_blocks_and_early_stop_do_not_become_success() {
    let mut encoder = ChatStreamEncoder::new(super::super::identity(), false);
    encoder.push_event(None, &start()).unwrap();
    assert_eq!(encoder.finish().unwrap_err(), ChatError::IncompleteStream);
    let block =
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}});
    encoder.push_event(None, &block).unwrap();
    assert_eq!(
        encoder.push_event(None, &block).unwrap_err(),
        ChatError::InvalidResponse
    );
    assert_eq!(
        encoder
            .push_event(None, &json!({"type":"message_stop"}))
            .unwrap_err(),
        ChatError::InvalidResponse
    );
}

#[test]
fn upstream_error_preserves_error_type_without_finish_chunk() {
    let mut encoder = ChatStreamEncoder::new(super::super::identity(), true);
    let error = json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}});
    assert_eq!(
        encoder.push_event(None, &error).unwrap(),
        vec![json!({"error":error["error"]})]
    );
    assert!(encoder.has_wire_failure());
    assert!(!encoder.is_completed());
    assert!(encoder.finish().unwrap().is_empty());
}
