//! Chat 原文解析与选路事实不依赖任何目标转换

use gateway_protocol::{
    chat_completions::{CHAT_PROTOCOL, ChatError, parse_request},
    request::{request_facts, request_observation},
};
use serde_json::json;

#[test]
fn parsing_retains_original_protocol_and_usage_preference() {
    let value = json!({"model":"m","messages":[{"role":"user","content":"hello"}],"stream":true,"stream_options":{"include_usage":true},"future":{"store":true}});
    let parsed = parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(parsed.body, value.as_object().unwrap().clone());
    assert!(parsed.include_usage);
    assert!(!parsed.body.contains_key("input"));
}

#[test]
fn facts_are_extracted_from_chat_fields_directly() {
    let value = json!({"model":"m","max_tokens":12,"max_completion_tokens":34,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}]}],"tools":[{"type":"function","function":{"name":"f"}}],"reasoning_effort":"high","response_format":{"type":"json_schema"}});
    let facts = request_facts(CHAT_PROTOCOL, value.as_object().unwrap());
    assert_eq!(facts.max_output_tokens, Some(34));
    assert!(facts.vision && facts.tools && facts.reasoning && facts.json_schema);
    assert!(!facts.native_continuation);
    assert_eq!(
        request_observation(
            CHAT_PROTOCOL,
            value.as_object().unwrap(),
            &Default::default(),
            None
        )
        .reasoning_effort
        .as_deref(),
        Some("high")
    );
}

#[test]
fn invalid_envelopes_and_stream_options_keep_safe_field_errors() {
    assert_eq!(parse_request(b"[").err(), Some(ChatError::MalformedJson));
    assert_eq!(parse_request(b"[]").err(), Some(ChatError::ExpectedObject));
    for (field, value) in [
        ("n", json!(2)),
        ("stream_options", json!({"include_usage":true})),
    ] {
        let mut request = super::request();
        request.insert(field.into(), value);
        let error = parse_request(&serde_json::to_vec(&request).unwrap())
            .err()
            .unwrap();
        assert_eq!(error.parameter(), Some(field));
    }
}

#[test]
fn invalid_tool_history_reports_source_path() {
    let request = json!({"model":"m","messages":[{"role":"tool","content":"ok"}]});
    assert_eq!(
        parse_request(&serde_json::to_vec(&request).unwrap())
            .err()
            .unwrap()
            .parameter(),
        Some("messages[0].tool_call_id")
    );
}

#[test]
fn unsupported_content_is_rejected_before_any_target_is_selected() {
    for (role, part) in [
        ("user", json!({"type":"SENSITIVE_CONTENT"})),
        (
            "assistant",
            json!({"type":"image_url","image_url":{"url":"https://example.invalid/private"}}),
        ),
    ] {
        let request = json!({"model":"m","messages":[{"role":role,"content":[part]}]});
        let error = parse_request(&serde_json::to_vec(&request).unwrap())
            .err()
            .unwrap();
        assert_eq!(error.parameter(), Some("messages[0].content[0].type"));
        assert!(!error.to_string().contains("SENSITIVE_CONTENT"));
    }
}
