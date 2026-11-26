//! Chat 原文解析与选路事实不依赖任何目标转换

use gateway_protocol::{
    chat_completions::{CHAT_PROTOCOL, ChatError, genai, messages, parse_request, responses},
    request::{request_facts, request_observation},
};
use serde_json::json;

#[test]
fn parsing_retains_original_protocol_and_usage_preference() {
    let value = json!({"model":"m","messages":[{"role":"user","content":"hello"}],"stream":true,"stream_options":{"include_usage":true},"future":{"store":true}});
    let parsed = parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(parsed, value.as_object().unwrap().clone());
    assert!(!parsed.contains_key("input"));
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
fn invalid_envelopes_and_chat_controls_keep_safe_field_errors() {
    assert_eq!(parse_request(b"[").err(), Some(ChatError::MalformedJson));
    assert_eq!(parse_request(b"[]").err(), Some(ChatError::ExpectedObject));
    for (field, value) in [
        ("n", json!(2)),
        ("store", json!(true)),
        ("previous_response_id", json!("resp_previous")),
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

#[test]
fn capability_facts_only_read_protocol_declarations() {
    let mut body = super::request();
    body.insert("tools".into(), json!([{"type":"function","function":{"name":"save","parameters":{"type":"object","properties":{"payload":{"type":"object","default":{"type":"image_generation"}}},"examples":[{"type":"image_generation"}]}}}]));
    body.insert(
        "messages".into(),
        json!([{"role":"user","content":"hello","metadata":{"type":"image_url"}}]),
    );
    let facts = request_facts(CHAT_PROTOCOL, &body);
    assert!(!facts.image_generation && !facts.vision);
    assert!(facts.tools);
    body["tools"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"image_generation"}));
    assert!(request_facts(CHAT_PROTOCOL, &body).image_generation);
    body.remove("tools");
    body.insert("web_search_options".into(), json!(null));
    assert!(!request_facts(CHAT_PROTOCOL, &body).tools);
}

#[test]
fn all_targets_apply_the_same_common_request_validation() {
    for (field, value, parameter) in [
        (
            "messages",
            json!([{"role":"assistant","content":"ok","refusal":17}]),
            "messages[0].refusal",
        ),
        (
            "messages",
            json!([{"role":"user","content":"ok","name":17}]),
            "messages[0].name",
        ),
        (
            "messages",
            json!([{"role":"user","content":"ok","tool_calls":[]}]),
            "messages[0].tool_calls",
        ),
        (
            "messages",
            json!([{"role":"assistant","tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments":17}}]}]),
            "messages[0].tool_calls[0].function.arguments",
        ),
        ("n", json!(2), "n"),
        (
            "stream_options",
            json!({"include_usage":true}),
            "stream_options",
        ),
    ] {
        let mut body = super::request();
        body.insert(field.into(), value);
        let expected = parse_request(&serde_json::to_vec(&body).unwrap())
            .err()
            .unwrap();
        assert_eq!(expected.parameter(), Some(parameter));
        for result in [
            responses::convert_request(&body),
            messages::convert_request(&body, &Default::default()),
            genai::convert_request(&body),
        ] {
            assert_eq!(result.unwrap_err(), expected);
        }
    }
}

#[test]
fn custom_format_constraints_are_preserved_or_rejected_by_the_target() {
    for format in [
        json!(null),
        json!({"type":"text"}),
        json!({"type":"grammar","grammar":{"syntax":"lark","definition":"PRIVATE_GRAMMAR"}}),
        json!(17),
    ] {
        let mut body = super::request();
        body.insert(
            "tools".into(),
            json!([{"type":"custom","custom":{"name":"run","format":format}}]),
        );
        let responses = responses::convert_request(&body).unwrap();
        assert_eq!(responses["tools"][0]["format"], format);
        for result in [
            messages::convert_request(&body, &Default::default()),
            genai::convert_request(&body),
        ] {
            if format.is_null() || format["type"] == "text" {
                assert!(result.is_ok());
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.parameter(), Some("tools[0].custom.format"));
                assert!(!error.to_string().contains("PRIVATE_GRAMMAR"));
            }
        }
    }
}
