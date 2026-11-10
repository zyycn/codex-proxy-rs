//! 验证聚合器的未知字段、累计用量与截断检查

use gateway_protocol::anthropic::MessagesAccumulator;
use serde_json::json;

#[test]
fn accumulator_preserves_native_fields_and_merges_usage() {
    let mut stream = MessagesAccumulator::default();
    for event in [
        json!({"type":"message_start","message":{"id":"m1","type":"message","content":[],"future_field":{"enabled":true},"usage":{"input_tokens":4,"cache_read_input_tokens":5}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"lookup","input":{},"future_tool_field":7}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"hi\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":9}}),
    ] {
        assert!(stream.push(&event).expect("聚合事件").is_none());
    }
    let message = stream
        .push(&json!({"type":"message_stop"}))
        .expect("正常结束")
        .expect("完整消息");
    stream.finish().expect("终态完整");
    assert_eq!(message["future_field"]["enabled"], true);
    assert_eq!(message["content"][0]["future_tool_field"], 7);
    assert_eq!(message["content"][0]["input"], json!({"q":"hi"}));
    assert_eq!(
        message["usage"],
        json!({"input_tokens":4,"cache_read_input_tokens":5,"output_tokens":9})
    );
}

#[test]
fn explicit_errors_finish_but_transport_truncation_does_not() {
    let mut stream = MessagesAccumulator::default();
    assert!(stream.finish().is_err());
    let error = json!({"type":"error","error":{"type":"api_error","message":"Interrupted."}});
    assert_eq!(stream.push(&error).expect("错误终态"), Some(error));
    assert!(stream.finish().is_ok());
    assert!(stream.push(&json!({"type":"message_stop"})).is_err());
}

#[test]
fn native_json_rejects_unparseable_tool_input_at_message_stop() {
    let mut stream = MessagesAccumulator::default();
    for event in [
        json!({"type":"message_start","message":{"content":[]}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
    ] {
        stream.push(&event).expect("输入分片");
    }
    assert!(stream.finish().is_err());
    assert!(stream.push(&json!({"type":"message_stop"})).is_err());
}
