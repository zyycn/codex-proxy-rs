//! 验证完整响应的停止语义与错误终态

use gateway_protocol::anthropic::{messages_to_responses_response, responses_to_messages_response};
use serde_json::{Map, json};

#[test]
fn stop_reasons_distinguish_incomplete_refusal_pause_and_tools() {
    for (reason, status, incomplete) in [
        ("max_tokens", "incomplete", Some("max_output_tokens")),
        (
            "model_context_window_exceeded",
            "incomplete",
            Some("max_output_tokens"),
        ),
        ("refusal", "incomplete", Some("content_filter")),
        ("pause_turn", "completed", None),
        ("tool_use", "completed", None),
        ("end_turn", "completed", None),
    ] {
        let converted = messages_to_responses_response(
            &json!({"id":"m1","content":[],"stop_reason":reason,"usage":{}}),
            &Map::new(),
        )
        .expect("停止转换");
        assert_eq!(converted["status"], status);
        assert_eq!(
            converted
                .pointer("/incomplete_details/reason")
                .and_then(|v| v.as_str()),
            incomplete
        );
        assert_eq!(
            converted.get("end_turn").and_then(|v| v.as_bool()),
            (reason == "pause_turn").then_some(false)
        );
    }
}

#[test]
fn upstream_errors_remain_errors_in_both_directions() {
    let response = messages_to_responses_response(
        &json!({"type":"error","error":{"type":"overloaded_error","message":"Busy."}}),
        &Map::new(),
    )
    .expect("错误转换");
    assert_eq!(response["status"], "failed");
    let message = responses_to_messages_response(&response, &Map::new()).expect("错误回转");
    assert_eq!(message["type"], "error");
    assert_eq!(message["error"]["message"], "Busy.");
}
