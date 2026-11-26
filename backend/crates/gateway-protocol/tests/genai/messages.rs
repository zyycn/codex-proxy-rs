//! 验证转换路径的工具往返、多模态输入和签名来源

use gateway_protocol::{
    anthropic::{MessagesAccumulator, MessagesToResponsesStream},
    genai::{GenAiToMessagesStream, messages_to_genai_request, responses_to_genai_request},
};
use serde_json::{Value, json};

#[test]
fn signed_tool_roundtrip_preserves_custom_identity_and_final_usage() {
    let original =
        json!({"model":"fixture","input":"Use patch","tools":[{"type":"custom","name":"patch"}]});
    let mut native = GenAiToMessagesStream::default();
    let mut responses = MessagesToResponsesStream::new(original.as_object().unwrap());
    let first = json!({"responseId":"r","candidates":[{"content":{"parts":[{"functionCall":{"name":"patch","args":{"input":"abc"}},"thoughtSignature":"opaque-signature"}]},"finishReason":"STOP"}]});
    let mut emitted = Vec::new();
    for event in native.push(&first).unwrap() {
        emitted.extend(responses.push(&event).unwrap());
    }
    assert!(!emitted.iter().any(|e| e["type"] == "response.completed"));
    for event in native.push(&json!({"usageMetadata":{"promptTokenCount":10,"cachedContentTokenCount":4,"candidatesTokenCount":3,"thoughtsTokenCount":2}})).unwrap().into_iter().chain(native.finish().unwrap()) {
        emitted.extend(responses.push(&event).unwrap());
    }
    let response = &emitted.last().unwrap()["response"];
    assert_eq!(response["usage"]["input_tokens"], 10);
    assert_eq!(response["usage"]["output_tokens"], 5);
    let mut input = response["output"].as_array().unwrap().clone();
    let call = input
        .iter()
        .find(|e| e["type"] == "custom_tool_call")
        .unwrap();
    assert_eq!(call["input"], "abc");
    let call_id = call["call_id"].clone();
    input.push(json!({"type":"custom_tool_call_output","call_id":call_id,"output":"done"}));
    let next = json!({"model":"fixture","input":input,"tools":original["tools"]});
    let request = responses_to_genai_request(next.as_object().unwrap()).unwrap();
    let call = &request["contents"][0]["parts"][0];
    assert_eq!(call["thoughtSignature"], "opaque-signature");
    assert_eq!(call["functionCall"]["name"], "patch");
    assert_eq!(
        request["contents"][1]["parts"][0]["functionResponse"]["name"],
        "patch"
    );
}

#[test]
fn messages_replay_keeps_late_signatures_and_rejects_incomplete_stream() {
    let mut native = GenAiToMessagesStream::default();
    let mut messages = MessagesAccumulator::default();
    for frame in [
        json!({"candidates":[{"content":{"parts":[{"text":"reason","thought":true}]}}]}),
        json!({"candidates":[{"content":{"parts":[{"thoughtSignature":"late"},{"text":"answer"}]},"finishReason":"STOP"}]}),
    ] {
        for event in native.push(&frame).unwrap() {
            messages.push(&event).unwrap();
        }
    }
    let mut complete = None;
    for event in native.finish().unwrap() {
        complete = messages.push(&event).unwrap().or(complete);
    }
    let complete = complete.unwrap();
    let request = json!({"messages":[{"role":"assistant","content":complete["content"]}]});
    let body = messages_to_genai_request(request.as_object().unwrap()).unwrap();
    assert_eq!(body["contents"][0]["parts"][0]["thoughtSignature"], "late");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "reason");
    let mut incomplete = GenAiToMessagesStream::default();
    incomplete
        .push(&json!({"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}))
        .unwrap();
    assert!(incomplete.finish().is_err());
}

#[test]
fn maps_system_images_schema_and_forced_tools_without_dropping_unknown_content() {
    let request = json!({"system":"system","messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"image"}}]}],"tools":[{"name":"lookup","input_schema":{"type":"object"}}],"tool_choice":{"type":"tool","name":"lookup"},"output_config":{"format":{"type":"json_schema","schema":{"type":"object"}}}});
    let body = messages_to_genai_request(request.as_object().unwrap()).unwrap();
    assert_eq!(
        body["contents"][0]["parts"][0]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
        json!(["lookup"])
    );
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(
        body["generationConfig"]["responseSchema"],
        json!({"type":"object"})
    );
    let invalid = json!({"messages":[{"role":"user","content":[{"type":"unknown"}]}]});
    assert!(messages_to_genai_request(invalid.as_object().unwrap()).is_err());
    let _: Value = request;
}

#[test]
fn merges_text_frames_and_omits_unsigned_empty_tool_placeholders() {
    let mut stream = GenAiToMessagesStream::default();
    let mut messages = MessagesAccumulator::default();
    for frame in [
        json!({"candidates":[{"content":{"parts":[{"text":"first "}]}}]}),
        json!({"candidates":[{"content":{"parts":[{"text":"second"},{"thoughtSignature":"text-signature"},{"text":""}]}}]}),
        json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"call","name":"lookup","args":{}}},{"text":""}]},"finishReason":"OTHER"}]}),
    ] {
        for event in stream.push(&frame).unwrap() {
            messages.push(&event).unwrap();
        }
    }
    let mut complete = None;
    for event in stream.finish().unwrap() {
        complete = messages.push(&event).unwrap().or(complete);
    }
    let complete = complete.unwrap();
    assert_eq!(complete["stop_reason"], "tool_use");
    assert_eq!(complete["content"][0]["text"], "first second");
    let request = json!({"messages":[{"role":"assistant","content":complete["content"]}]});
    let native = messages_to_genai_request(request.as_object().unwrap()).unwrap();
    let parts = native["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert!(parts[0].get("thoughtSignature").is_none());
    assert_eq!(parts[1]["functionCall"]["name"], "lookup");
}

#[test]
fn visible_text_streams_as_one_block_without_a_trailing_signature_carrier() {
    for late in [false, true] {
        let mut stream = GenAiToMessagesStream::default();
        let mut accumulator = MessagesAccumulator::default();
        let mut events = stream
            .push(&json!({"candidates":[{"content":{"parts":[{"text":"first "}]}}]}))
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event["delta"]["text"] == "first ")
        );
        let mut part = json!({"text":"answer"});
        if !late {
            part["thoughtSignature"] = json!("opaque");
        }
        events.extend(
            stream
                .push(&json!({"candidates":[{"content":{"parts":[part]}}]}))
                .unwrap(),
        );
        if late {
            events.extend(stream.push(&json!({"candidates":[{"content":{"parts":[{"thoughtSignature":"opaque"}]}}]})).unwrap());
        }
        events.extend(
            stream
                .push(&json!({"candidates":[{"finishReason":"STOP"}]}))
                .unwrap(),
        );
        events.extend(stream.finish().unwrap());
        let mut complete = None;
        for event in events {
            complete = accumulator.push(&event).unwrap().or(complete);
        }
        let complete = complete.unwrap();
        assert_eq!(
            complete["content"],
            json!([{"type":"text","text":"first answer"}])
        );
        let request = json!({"messages":[{"role":"assistant","content":complete["content"]}]});
        let native = messages_to_genai_request(request.as_object().unwrap()).unwrap();
        assert_eq!(
            native["contents"][0]["parts"],
            json!([{"text":"first answer"}])
        );
    }
}

#[test]
fn native_signature_carriers_do_not_reach_anthropic_upstreams() {
    let mut request = json!({"messages":[{"role":"assistant","content":[
        {"type":"thinking","thinking":"private","signature":"cpr:genai:v1:{}"},
        {"type":"thinking","thinking":"native","signature":"anthropic-signature"},
        {"type":"text","text":"visible"}
    ]}]})
    .as_object()
    .unwrap()
    .clone();
    gateway_protocol::anthropic::strip_foreign_thinking_for_messages(&mut request);
    assert_eq!(
        request["messages"][0]["content"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        request["messages"][0]["content"][0]["signature"],
        "anthropic-signature"
    );
}

#[test]
fn unsigned_tool_history_uses_documented_fallback_without_a_signature_cache() {
    let request = json!({"messages":[
        {"role":"assistant","content":[{"type":"text","text":""},{"type":"tool_use","id":"call","name":"lookup","input":{}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"value"}]}
    ]});
    let native = messages_to_genai_request(request.as_object().unwrap()).unwrap();
    assert_eq!(
        native["contents"][0]["parts"][0]["thoughtSignature"],
        "skip_thought_signature_validator"
    );
    assert_eq!(
        native["contents"][1]["parts"][0]["functionResponse"]["id"],
        "call"
    );
}

#[test]
fn empty_client_placeholders_do_not_change_message_roles_or_system_content() {
    let request = json!({"system":"instructions","messages":[
        {"role":"user","content":"hello"},{"role":"system","content":[]}
    ]});
    let native = messages_to_genai_request(request.as_object().unwrap()).unwrap();
    assert_eq!(
        native["contents"],
        json!([{"role":"user","parts":[{"text":"hello"}]}])
    );
    assert_eq!(
        native["systemInstruction"]["parts"][0]["text"],
        "instructions"
    );
    let invalid = json!({"messages":[{"role":"system","content":"instructions"}]});
    assert!(messages_to_genai_request(invalid.as_object().unwrap()).is_err());
}
