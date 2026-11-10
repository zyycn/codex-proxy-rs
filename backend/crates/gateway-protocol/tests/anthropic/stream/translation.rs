//! 验证双向增量输出与完整 JSON 语义一致

use gateway_protocol::anthropic::{
    MessagesAccumulator, MessagesToResponsesStream, ResponsesToMessagesStream,
    messages_to_responses_response, responses_to_messages_response,
};
use serde_json::{Map, Value, json};

#[test]
fn messages_stream_emits_live_text_and_final_cache_usage() {
    let request = Map::from_iter([("model".into(), json!("alias"))]);
    let mut stream = MessagesToResponsesStream::new(&request);
    let mut events = Vec::new();
    for event in [
        json!({"type":"message_start","message":{"id":"m1","model":"upstream","content":[],"usage":{"input_tokens":4,"output_tokens":0,"cache_read_input_tokens":8}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ] {
        events.extend(stream.push(&event).expect("转换事件"));
    }
    stream.finish().expect("终态完整");
    assert!(events.iter().any(|event|event["type"]=="response.output_text.delta" && event["delta"]=="hello"));
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["sequence_number"], index);
    }
    let final_response = &events.last().expect("结束事件")["response"];
    let expected=messages_to_responses_response(&json!({"id":"m1","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":4,"output_tokens":3,"cache_read_input_tokens":8}}),&request).expect("JSON响应");
    assert_eq!(final_response, &expected);
}

#[test]
fn custom_tools_use_custom_events_and_restore_string_input() {
    let request = json!({"model":"alias","tools":[{"type":"custom","name":"shell"}]})
        .as_object()
        .expect("请求对象")
        .clone();
    let mut stream = MessagesToResponsesStream::new(&request);
    let mut events = Vec::new();
    for event in [
        json!({"type":"message_start","message":{"id":"m1","content":[],"usage":{}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"shell","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"pwd\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{}}),
        json!({"type":"message_stop"}),
    ] {
        events.extend(stream.push(&event).expect("工具转换"));
    }
    assert!(events.iter().any(
        |event| event["type"] == "response.custom_tool_call_input.delta" && event["delta"] == "pwd"
    ));
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "response.function_call_arguments.delta")
    );
    assert_eq!(
        events.last().expect("结束事件")["response"]["output"][0]["input"],
        "pwd"
    );
}

#[test]
fn responses_thinking_uses_final_signature_without_replaying_text() {
    let request = Map::from_iter([("model".into(), json!("alias"))]);
    let item = json!({"type":"reasoning","id":"reason1","summary":[{"type":"summary_text","text":"thought"}],"encrypted_content":"gAAAAsigned"});
    let response = json!({"id":"r1","model":"upstream","status":"completed","output":[item],"usage":{"input_tokens":12,"output_tokens":4,"input_tokens_details":{"cached_tokens":2}}});
    let mut stream = ResponsesToMessagesStream::new(&request);
    let mut events = Vec::new();
    for event in [
        json!({"type":"response.created","response":{"id":"r1"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"reason1","summary":[]}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"thought"}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":response}),
    ] {
        events.extend(stream.push(&event).expect("思考转换"));
    }
    stream.finish().expect("终态完整");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.pointer("/delta/type") == Some(&json!("thinking_delta")))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.pointer("/delta/type") == Some(&json!("signature_delta")))
            .count(),
        1
    );
    let mut accumulator = MessagesAccumulator::default();
    let mut final_message = None;
    for event in events {
        if let Some(message) = accumulator.push(&event).expect("聚合转换流") {
            final_message = Some(message);
        }
    }
    assert_eq!(
        final_message.expect("终态消息"),
        responses_to_messages_response(&response, &request).expect("完整响应转换")
    );
}

#[test]
fn responses_json_only_terminal_synthesizes_content_and_stops() {
    let request = Map::from_iter([("model".into(), json!("alias"))]);
    let response = json!({"id":"r1","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"partial"}]}],"usage":{"input_tokens":2,"output_tokens":1}});
    let mut stream = ResponsesToMessagesStream::new(&request);
    let events = stream
        .push(&json!({"type":"response.incomplete","response":response}))
        .expect("完整终态");
    let mut accumulator = MessagesAccumulator::default();
    let mut final_message = Value::Null;
    for event in events {
        if let Some(message) = accumulator.push(&event).expect("聚合消息") {
            final_message = message;
        }
    }
    assert_eq!(final_message["content"][0]["text"], "partial");
    assert_eq!(final_message["stop_reason"], "max_tokens");
    stream.finish().expect("不完整生成也是完整协议终态");
}

#[test]
fn explicit_errors_and_truncated_streams_are_distinct() {
    let mut to_messages = ResponsesToMessagesStream::new(&Map::new());
    assert!(to_messages.finish().is_err());
    let events=to_messages.push(&json!({"type":"response.failed","response":{"error":{"type":"server_error","message":"Failed."}}})).expect("错误转换");
    assert_eq!(events[0]["type"], "error");
    assert!(to_messages.finish().is_ok());
    let mut to_responses = MessagesToResponsesStream::new(&Map::new());
    assert!(to_responses.finish().is_err());
    let events = to_responses
        .push(&json!({"type":"error","error":{"type":"overloaded_error","message":"Busy."}}))
        .expect("错误转换");
    assert_eq!(events[0]["type"], "response.failed");
    assert!(to_responses.finish().is_ok());
}

#[test]
fn interrupted_function_arguments_wait_for_stop_reason_and_keep_raw_input() {
    for reason in ["max_tokens", "model_context_window_exceeded", "tool_use"] {
        let mut stream = MessagesToResponsesStream::new(&Map::new());
        let mut events = Vec::new();
        let arguments = "{\n  \"value\": \"未完成";
        for event in [
            json!({"type":"message_start","message":{"id":"m1","content":[],"usage":{"input_tokens":7}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"lookup","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":arguments}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"partial answer"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":5}}),
        ] {
            events.extend(stream.push(&event).expect("等待消息终态"));
        }
        assert!(stream.finish().is_err());
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "response.function_call_arguments.done")
        );
        events.extend(
            stream
                .push(&json!({"type":"message_stop"}))
                .expect("保留上游终态"),
        );
        stream.finish().expect("工具输入不影响协议终态");
        let final_event = events.last().unwrap();
        let expected_status = if reason == "tool_use" {
            "completed"
        } else {
            "incomplete"
        };
        assert_eq!(final_event["type"], format!("response.{expected_status}"));
        if reason != "tool_use" {
            assert_eq!(
                final_event["response"]["incomplete_details"]["reason"],
                "max_output_tokens"
            );
        }
        assert_eq!(final_event["response"]["output"][0]["arguments"], arguments);
        assert_eq!(
            final_event["response"]["output"][0]["status"],
            expected_status
        );
        assert_eq!(
            final_event["response"]["output"][1]["content"][0]["text"],
            "partial answer"
        );
        assert_eq!(final_event["response"]["usage"]["output_tokens"], 5);
        assert!(events.iter().any(|event| event["type"]
            == "response.function_call_arguments.done"
            && event["arguments"] == arguments));
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event["sequence_number"], index);
        }
    }
}

#[test]
fn incomplete_custom_input_preserves_decoded_prefix_and_unfinished_escapes() {
    let request = json!({"tools":[{"type":"custom","name":"shell"}]})
        .as_object()
        .unwrap()
        .clone();
    for (arguments, expected) in [
        (
            r#"{"input":"printf\n值\uD83D\uDE00tail\"#,
            "printf\n值😀tail\\",
        ),
        (r#"{"input":"x\uD83D\u"#, r"x\uD83D\u"),
        (r#"{"input":"finished""#, "finished"),
        ("{", "{"),
    ] {
        let mut stream = MessagesToResponsesStream::new(&request);
        let mut events = Vec::new();
        for event in [
            json!({"type":"message_start","message":{"id":"m1","content":[],"usage":{}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"shell","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":arguments}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{}}),
            json!({"type":"message_stop"}),
        ] {
            events.extend(stream.push(&event).expect("截断 custom 工具"));
        }
        let item = &events.last().unwrap()["response"]["output"][0];
        assert_eq!(item["type"], "custom_tool_call");
        assert_eq!(item["status"], "incomplete");
        assert_eq!(item["input"], expected);
        assert!(events.iter().any(|event| event["type"]
            == "response.custom_tool_call_input.delta"
            && event["delta"] == expected));
    }
}

#[test]
fn complete_function_arguments_keep_the_original_serialization() {
    let arguments = "{ \"number\": 1.00, \"text\": \"\\u0061\" }";
    let mut stream = MessagesToResponsesStream::new(&Map::new());
    let mut events = Vec::new();
    for event in [
        json!({"type":"message_start","message":{"id":"m1","content":[],"usage":{}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":arguments}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{}}),
        json!({"type":"message_stop"}),
    ] {
        events.extend(stream.push(&event).unwrap());
    }
    assert!(events.iter().any(
        |event| event["type"] == "response.function_call_arguments.done"
            && event["arguments"] == arguments
    ));
    assert_eq!(
        events.last().unwrap()["response"]["output"][0]["arguments"],
        arguments
    );
}

#[test]
fn terminal_uses_delivered_tool_when_output_is_not_repeated() {
    for (terminal, expected) in [
        (json!({"id":"r","status":"completed"}), "tool_use"),
        (
            json!({"id":"r","status":"completed","output":[]}),
            "tool_use",
        ),
        (
            json!({"id":"r","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}),
            "max_tokens",
        ),
    ] {
        let mut stream = ResponsesToMessagesStream::new(&Map::new());
        let done=stream.push(&json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"c","name":"read","arguments":"{}"}})).unwrap();
        assert!(
            done.iter()
                .any(|e| e.pointer("/content_block/type") == Some(&json!("tool_use")))
        );
        let events=stream.push(&json!({"type":format!("response.{}",terminal["status"].as_str().unwrap()),"response":terminal})).unwrap();
        assert_eq!(events[0]["delta"]["stop_reason"], expected);
        assert_eq!(events[1]["type"], "message_stop");
    }
}
