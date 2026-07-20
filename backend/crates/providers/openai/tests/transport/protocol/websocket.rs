use provider_openai::transport::protocol::responses::CodexResponsesRequest;
use provider_openai::transport::protocol::websocket::{
    OpeningAuditSnapshot, is_terminal_websocket_event, websocket_audit_artifact_from_attempt,
    websocket_event_to_sse_frame, websocket_metadata_turn_state, websocket_payload_audit_snapshot,
    websocket_response_completed_id, websocket_response_create_payload_text,
};
use serde_json::json;

#[test]
fn websocket_payload_audit_should_redact_sensitive_content_and_preserve_key_order() {
    let mut request = CodexResponsesRequest::from_body(
        json!({
            "model": "gpt-test",
            "instructions": "private instructions",
            "input": [{"role": "user", "content": "private prompt"}],
            "tools": [{"type": "function", "name": "private-tool"}],
            "service_tier": "flex"
        })
        .as_object()
        .expect("request object")
        .clone(),
    );
    request.set_prompt_cache_key(Some("cache-secret".to_owned()));
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
    let mut request = CodexResponsesRequest::new_http_sse(
        "gpt-test",
        "capture instructions",
        vec![json!({"role": "user", "content": "capture prompt"})],
    );
    request.set_prompt_cache_key(Some("session-1".to_owned()));
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
    let request = CodexResponsesRequest::new_http_sse("gpt-test", "", Vec::new());
    let payload = websocket_response_create_payload_text(&request).expect("serialize payload");
    let value = serde_json::from_str::<serde_json::Value>(&payload).expect("payload JSON");

    assert_eq!(value["instructions"], "");
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
fn websocket_metadata_turn_state_should_accept_case_insensitive_header() {
    let event = json!({
        "type": "response.metadata",
        "headers": {"X-Codex-Turn-State": ["turn-from-metadata"]}
    })
    .to_string();

    assert_eq!(
        websocket_metadata_turn_state(&event).as_deref(),
        Some("turn-from-metadata")
    );
}

#[test]
fn websocket_terminal_event_should_cover_all_current_terminal_types() {
    for event in [
        "response.completed",
        "response.incomplete",
        "response.failed",
        "error",
    ] {
        assert!(is_terminal_websocket_event(event));
    }
    assert!(!is_terminal_websocket_event("response.output_text.delta"));
}

#[test]
fn websocket_completed_id_should_validate_the_official_shape() {
    let valid = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_valid",
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    })
    .to_string();
    let invalid_usage = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_invalid",
            "usage": {"input_tokens": "bad", "output_tokens": 1, "total_tokens": 1}
        }
    })
    .to_string();

    assert_eq!(
        websocket_response_completed_id(&valid).expect("valid completed"),
        Some("resp_valid".to_owned())
    );
    assert!(
        websocket_response_completed_id(&invalid_usage)
            .expect_err("invalid usage")
            .contains("failed to parse ResponseCompleted")
    );
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
    let mut request = CodexResponsesRequest::new_http_sse(
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
fn websocket_completed_id_should_reject_missing_id_and_incomplete_usage() {
    let missing_id = json!({
        "type": "response.completed",
        "response": {
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    })
    .to_string();
    let incomplete_usage = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_incomplete_usage",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }
    })
    .to_string();

    assert!(
        websocket_response_completed_id(&missing_id)
            .expect_err("missing id")
            .contains("missing field")
    );
    assert!(
        websocket_response_completed_id(&incomplete_usage)
            .expect_err("incomplete usage")
            .contains("missing field")
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
