use provider_openai::transport::protocol::responses::CodexResponsesRequest;
use provider_openai::transport::protocol::websocket::{
    OpeningAuditSnapshot, websocket_audit_artifact_from_attempt, websocket_event_to_sse_frame,
    websocket_metadata_turn_state, websocket_payload_audit_snapshot,
    websocket_response_completed_id, websocket_response_create_payload_text,
};
use serde_json::json;

use super::super::{codex_request, codex_request_with_prompt_cache_key};

#[test]
fn websocket_payload_audit_should_redact_sensitive_content_and_preserve_key_order() {
    let mut request = CodexResponsesRequest::from_body(
        json!({
            "model": "gpt-test",
            "instructions": "private instructions",
            "input": [{"role": "user", "content": "private prompt"}],
            "tools": [{"type": "function", "name": "private-tool"}],
            "service_tier": "flex",
            "prompt_cache_key": "cache-secret"
        })
        .as_object()
        .expect("request object")
        .clone(),
    );
    request.set_client_metadata(Some(json!({"thread_id": "thread-secret"})));

    let snapshot = websocket_payload_audit_snapshot(&request);

    assert_eq!(
        snapshot.top_level_keys,
        vec![
            "type",
            "model",
            "instructions",
            "input",
            "tools",
            "service_tier",
            "prompt_cache_key",
            "client_metadata",
        ]
    );
    assert_eq!(snapshot.body["type"], "response.create");
    assert_eq!(snapshot.body["model"], "gpt-test");
    for field in [
        "instructions",
        "input",
        "tools",
        "prompt_cache_key",
        "client_metadata",
    ] {
        assert_eq!(snapshot.body[field], "<redacted>");
    }
}

#[test]
fn websocket_response_create_payload_should_preserve_transparent_body_order() {
    let mut request = codex_request_with_prompt_cache_key(
        "gpt-test",
        "capture instructions",
        vec![json!({"role": "user", "content": "capture prompt"})],
        "session-1",
    );
    request.set_client_metadata(Some(json!({"thread_id": "capture-thread"})));

    let payload = websocket_response_create_payload_text(&request).expect("serialize payload");
    let fields = [
        "\"type\":\"response.create\"",
        "\"model\":\"gpt-test\"",
        "\"instructions\":\"capture instructions\"",
        "\"input\":",
        "\"prompt_cache_key\":\"session-1\"",
        "\"client_metadata\":",
    ];
    let mut cursor = 0;
    for field in fields {
        let offset = payload[cursor..]
            .find(field)
            .unwrap_or_else(|| panic!("missing ordered field {field}"));
        cursor += offset + field.len();
    }
}

#[test]
fn websocket_response_create_payload_should_keep_explicit_empty_instructions() {
    let request = codex_request("gpt-test", "", Vec::new());
    let payload = websocket_response_create_payload_text(&request).expect("serialize payload");
    let value = serde_json::from_str::<serde_json::Value>(&payload).expect("payload JSON");

    assert_eq!(value["instructions"], "");
}

#[test]
fn websocket_response_create_payload_text_should_match_merged_map_serialization() {
    // body 自带 type 键与未知字段：文本帧必须与「合并 Map 再序列化」逐字节一致。
    let request = CodexResponsesRequest::from_body(
        json!({
            "model": "gpt-test",
            "type": "client-supplied",
            "x_unknown": {"nested": [1, 2.5, "3"]},
            "input": []
        })
        .as_object()
        .expect("request object")
        .clone(),
    );

    let merged = serde_json::to_string(
        &provider_openai::transport::protocol::websocket::websocket_response_create_payload(
            &request,
        ),
    )
    .expect("merged payload");
    let streamed = websocket_response_create_payload_text(&request).expect("streamed payload");

    assert_eq!(streamed, merged);
}

#[test]
fn required_websocket_audit_should_forbid_http_fallback() {
    let request = CodexResponsesRequest::from_body(
        json!({
            "model": "gpt-test",
            "input": [],
            "stream": true,
            "store": false,
            "generate": false
        })
        .as_object()
        .expect("request object")
        .clone(),
    );
    let payload = websocket_payload_audit_snapshot(&request);
    let artifact =
        websocket_audit_artifact_from_attempt(&request, OpeningAuditSnapshot::default(), payload);

    assert_eq!(artifact.transport_mode, "explicit_websocket_warmup");
    assert!(!artifact.fallback_allowed);
}

#[test]
fn websocket_event_to_sse_should_forward_public_events_and_strip_internal_events() {
    let event = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_ws",
            "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6}
        }
    })
    .to_string();

    assert_eq!(
        websocket_event_to_sse_frame(&event).expect("public event"),
        format!("event: response.completed\ndata: {event}\n\n")
    );
    assert!(websocket_event_to_sse_frame(r#"{"type":"codex.rate_limits"}"#).is_none());
    assert!(websocket_event_to_sse_frame(r#"{"type":"response.metadata"}"#).is_none());
    assert!(websocket_event_to_sse_frame(r#"{"response":{}}"#).is_none());
    assert!(websocket_event_to_sse_frame("not-json").is_none());
}

#[test]
fn websocket_event_to_sse_should_add_missing_previous_response_recovery_code() {
    let event = json!({
        "type": "error",
        "status": 400,
        "error": {
            "type": "invalid_request_error",
            "message": "Invalid `previous_response_id`."
        }
    })
    .to_string();

    let frame = websocket_event_to_sse_frame(&event).expect("public error event");

    assert!(frame.contains(r#""code":"previous_response_not_found""#));
}

#[test]
fn websocket_metadata_turn_state_should_accept_case_insensitive_header() {
    let event = json!({
        "type": "response.metadata",
        "headers": {"X-Codex-Turn-State": ["turn-from-metadata"]}
    });

    assert_eq!(
        websocket_metadata_turn_state(&event).as_deref(),
        Some("turn-from-metadata")
    );
}

#[test]
fn websocket_metadata_turn_state_should_ignore_public_event_metadata() {
    let event = json!({
        "type": "future.business.event",
        "metadata": {"turn_state": "business-value"},
        "turn_state": "another-business-value"
    });

    assert_eq!(websocket_metadata_turn_state(&event), None);
    assert!(websocket_event_to_sse_frame(&event.to_string()).is_some());
}

#[test]
fn websocket_completed_id_should_read_the_id_without_validating_the_response_shape() {
    let valid = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_valid",
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    });
    let invalid_usage = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_invalid",
            "usage": {"input_tokens": "bad", "output_tokens": 1, "total_tokens": 1}
        }
    });

    assert_eq!(
        websocket_response_completed_id(&valid),
        Some("resp_valid".to_owned())
    );
    assert_eq!(
        websocket_response_completed_id(&invalid_usage),
        Some("resp_invalid".to_owned())
    );
}

#[test]
fn websocket_completed_id_should_preserve_an_empty_opaque_id() {
    let event = json!({
        "type": "response.completed",
        "response": {"id": ""}
    });

    assert_eq!(websocket_response_completed_id(&event), Some(String::new()));
}

#[test]
fn websocket_typed_events_should_remain_transparent_without_schema_filtering() {
    let malformed_delta = json!({
        "type": "response.output_text.delta",
        "delta": 42
    })
    .to_string();
    let frame = websocket_event_to_sse_frame(&malformed_delta)
        .expect("typed upstream event remains transparent");

    assert!(frame.contains("event: response.output_text.delta"));
    assert!(frame.contains(&malformed_delta));
}

#[test]
fn websocket_audit_artifact_should_record_opening_and_redacted_payload() {
    let mut request = codex_request(
        "gpt-test",
        "private instructions",
        vec![json!({"role": "user", "content": "private prompt"})],
    );
    request.set_previous_response_id(Some("resp_secret".to_owned()));
    let opening = OpeningAuditSnapshot {
        request_line: "GET /backend-api/codex/responses HTTP/1.1".to_owned(),
        header_order: vec!["Host".to_owned(), "authorization".to_owned()],
        headers: vec![
            provider_openai::transport::protocol::websocket::OpeningAuditHeader {
                name: "authorization".to_owned(),
                value: "<redacted>".to_owned(),
            },
        ],
    };
    let payload = websocket_payload_audit_snapshot(&request);

    let artifact = websocket_audit_artifact_from_attempt(&request, opening.clone(), payload);

    assert_eq!(artifact.opening, Some(opening));
    assert_eq!(
        artifact.payload.expect("payload").body["input"],
        "<redacted>"
    );
}

#[test]
fn websocket_completed_id_should_leave_unreadable_completion_untracked() {
    let missing_id = json!({
        "type": "response.completed",
        "response": {
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    });
    let incomplete_usage = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_incomplete_usage",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }
    });

    assert_eq!(websocket_response_completed_id(&missing_id), None);
    assert_eq!(
        websocket_response_completed_id(&incomplete_usage),
        Some("resp_incomplete_usage".to_owned())
    );
}

#[test]
fn websocket_optional_null_fields_should_remain_forwardable() {
    for event in [
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "id": null,
                "role": "assistant",
                "phase": null,
                "metadata": null,
                "content": [{"type": "output_text", "text": "hello"}]
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "custom_tool_call",
                "id": null,
                "status": null,
                "call_id": "call_1",
                "name": "render",
                "input": "{}"
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {"type": "context_compaction", "encrypted_content": null}
        }),
    ] {
        assert!(websocket_event_to_sse_frame(&event.to_string()).is_some());
    }
}
