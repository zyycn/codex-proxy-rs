use codex_proxy_core::models::model::{ModelConfig, ParsedModelName};
use codex_proxy_core::protocol::codex::events::{
    cooldown_with_jitter, extract_sse_usage, extract_usage, parse_rate_limit_headers,
    parse_rate_limits_event, rate_limit_quota, retry_after_seconds_from_body, RateLimitWindow,
    TokenUsage,
};
use codex_proxy_core::protocol::codex::responses::CodexResponsesRequest;
use codex_proxy_core::protocol::codex::websocket::{
    classify_websocket_error_frame, is_terminal_websocket_event,
    retry_after_seconds_from_wrapped_error_headers,
    websocket_agent_message_output_item_event_invalid_required_fields,
    websocket_audit_artifact_from_attempt,
    websocket_compaction_output_item_event_invalid_required_fields,
    websocket_custom_tool_call_output_item_event_invalid_required_fields,
    websocket_custom_tool_call_output_payload_item_event_invalid_required_fields,
    websocket_delta_event_missing_official_required_fields, websocket_event_shape_parse_error,
    websocket_event_to_sse_frame,
    websocket_function_call_output_item_event_invalid_required_fields,
    websocket_function_call_output_payload_item_event_invalid_required_fields,
    websocket_image_generation_call_output_item_event_invalid_required_fields,
    websocket_incomplete_response_reason,
    websocket_local_shell_call_output_item_event_invalid_required_fields,
    websocket_message_output_item_event_invalid_required_fields, websocket_metadata_turn_state,
    websocket_output_item_event_invalid_item_type_tag,
    websocket_output_item_event_invalid_metadata, websocket_output_item_event_missing_item,
    websocket_output_item_event_non_object_item, websocket_parity_diff,
    websocket_payload_audit_snapshot,
    websocket_reasoning_output_item_event_invalid_required_fields,
    websocket_reasoning_summary_part_added_missing_summary_index,
    websocket_response_completed_missing_response, websocket_response_completed_parse_error,
    websocket_response_created_missing_response,
    websocket_response_output_text_delta_missing_delta,
    websocket_tool_search_call_output_item_event_invalid_required_fields,
    websocket_tool_search_output_item_event_invalid_required_fields,
    websocket_web_search_call_output_item_event_invalid_required_fields, OpeningAuditHeader,
    OpeningAuditSnapshot, PayloadAuditSnapshot, WebSocketAuditErrorSnapshot,
    WebSocketErrorClassificationProfile,
};
use codex_proxy_core::protocol::openai::responses::{
    response_from_codex_sse, translate_response_to_codex, translate_response_to_compact,
    CollectedResponse, OpenAiResponsesRequest,
};
use codex_proxy_core::serving::responses::{
    apply_response_model_options, http_sse_fallback_allowed, transport_for_request, CodexTransport,
};
use serde_json::json;

#[test]
fn codex_responses_request_should_enable_http_sse_defaults() {
    let request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);

    assert!(request.stream);
}

#[test]
fn codex_responses_transport_should_prefer_websocket_without_history() {
    let request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketPreferred
    );
    assert!(http_sse_fallback_allowed(&request));
}

#[test]
fn codex_responses_transport_should_require_websocket_for_previous_response_id() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);
    request.previous_response_id = Some("resp_previous".to_string());

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketRequired
    );
    assert!(!http_sse_fallback_allowed(&request));
}

#[test]
fn codex_responses_transport_should_allow_forced_http_sse() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);
    request.previous_response_id = Some("resp_previous".to_string());
    request.use_websocket = true;
    request.force_http_sse = true;

    assert_eq!(transport_for_request(&request), CodexTransport::HttpSse);
    assert!(http_sse_fallback_allowed(&request));
}

#[test]
fn codex_responses_transport_flags_should_not_serialize_to_upstream_json() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);
    request.use_websocket = true;
    request.force_http_sse = true;

    let body = serde_json::to_value(&request).expect("request should serialize");

    assert!(body.get("use_websocket").is_none());
    assert!(body.get("useWebSocket").is_none());
    assert!(body.get("force_http_sse").is_none());
    assert!(body.get("forceHttpSse").is_none());
}

#[test]
fn codex_websocket_payload_audit_snapshot_should_redact_user_content() {
    let mut request = CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "private instructions",
        vec![json!({
            "role": "user",
            "content": "private prompt",
        })],
    );
    request.previous_response_id = Some("resp_secret".to_string());
    request.reasoning = Some(json!({"effort": "medium"}));
    request.tools = Some(vec![json!({
        "type": "function",
        "name": "private_tool",
        "description": "private schema",
    })]);
    request.text = Some(json!({"format": {"type": "text"}}));
    request.service_tier = Some("flex".to_string());
    request.prompt_cache_key = Some("cache-secret".to_string());
    request.generate = Some(false);
    request.include = Some(vec!["reasoning.encrypted_content".to_string()]);
    request.client_metadata = Some(json!({
        "thread_id": "thread-secret",
        "safe": "value",
    }));

    let snapshot = websocket_payload_audit_snapshot(&request);

    assert_eq!(
        snapshot.top_level_keys,
        vec![
            "type",
            "model",
            "instructions",
            "previous_response_id",
            "input",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning",
            "store",
            "stream",
            "include",
            "service_tier",
            "prompt_cache_key",
            "text",
            "generate",
            "client_metadata",
        ]
    );
    assert_eq!(snapshot.body["type"], "response.create");
    assert_eq!(snapshot.body["model"], "gpt-5.5");
    assert_eq!(snapshot.body["stream"], true);
    assert_eq!(snapshot.body["instructions"], "<redacted>");
    assert_eq!(snapshot.body["input"], "<redacted>");
    assert_eq!(snapshot.body["previous_response_id"], "<redacted>");
    assert_eq!(snapshot.body["prompt_cache_key"], "<redacted>");
    assert_eq!(snapshot.body["client_metadata"], "<redacted>");
    assert_eq!(snapshot.body["tools"], "<redacted>");
}

#[test]
fn codex_websocket_response_create_payload_text_should_preserve_old_field_order() {
    let mut request = CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "private capture instructions",
        vec![json!({
            "role": "user",
            "content": "private capture prompt",
        })],
    );
    request.prompt_cache_key = Some("session-1".to_string());
    request.generate = Some(false);
    request.client_metadata = Some(json!({
        "thread_id": "capture-thread-secret",
        "safe": "capture",
    }));

    let payload =
        codex_proxy_core::protocol::codex::websocket::websocket_response_create_payload_text(
            &request,
        )
        .expect("payload should serialize");

    assert_substrings_appear_in_order(
        &payload,
        &[
            "\"type\":\"response.create\"",
            "\"model\":\"gpt-5.5\"",
            "\"instructions\":\"private capture instructions\"",
            "\"input\":",
            "\"tools\":[]",
            "\"tool_choice\":\"auto\"",
            "\"parallel_tool_calls\":true",
            "\"reasoning\":null",
            "\"store\":false",
            "\"stream\":true",
            "\"include\":[]",
            "\"prompt_cache_key\":\"session-1\"",
            "\"generate\":false",
            "\"client_metadata\":",
        ],
    );
}

#[test]
fn codex_websocket_audit_artifact_should_record_transport_opening_and_payload() {
    let mut request = CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "private instructions",
        vec![json!({
            "role": "user",
            "content": "private prompt",
        })],
    );
    request.previous_response_id = Some("resp_secret".to_string());
    let opening = OpeningAuditSnapshot {
        request_line: "GET /backend-api/codex/responses HTTP/1.1".to_string(),
        header_order: vec!["Host".to_string(), "authorization".to_string()],
        headers: vec![
            OpeningAuditHeader {
                name: "Host".to_string(),
                value: "chatgpt.com".to_string(),
            },
            OpeningAuditHeader {
                name: "authorization".to_string(),
                value: "<redacted>".to_string(),
            },
        ],
    };
    let payload = websocket_payload_audit_snapshot(&request);

    let artifact = websocket_audit_artifact_from_attempt(&request, opening.clone(), payload);

    assert_eq!(artifact.transport_mode, "websocket_required");
    assert!(!artifact.fallback_allowed);
    assert_eq!(artifact.opening, Some(opening));
    assert_eq!(
        artifact.payload.expect("payload").body["input"],
        "<redacted>"
    );
    assert_eq!(artifact.error, None);
}

#[test]
fn codex_websocket_parity_diff_should_report_header_payload_and_error_differences() {
    let current = codex_proxy_core::protocol::codex::websocket::WebSocketAuditArtifact {
        transport_mode: "websocket_preferred".to_string(),
        fallback_allowed: true,
        opening: Some(OpeningAuditSnapshot {
            request_line: "GET /codex/responses HTTP/1.1".to_string(),
            header_order: vec!["Host".to_string(), "Connection".to_string()],
            headers: vec![OpeningAuditHeader {
                name: "Sec-WebSocket-Extensions".to_string(),
                value: "permessage-deflate; client_max_window_bits".to_string(),
            }],
        }),
        payload: Some(PayloadAuditSnapshot {
            top_level_keys: vec!["type".to_string(), "model".to_string()],
            body: json!({"type": "response.create"}),
        }),
        error: Some(WebSocketAuditErrorSnapshot {
            classification: "opening_failed".to_string(),
            message: "connection refused".to_string(),
        }),
    };
    let reference = codex_proxy_core::protocol::codex::websocket::WebSocketAuditArtifact {
        transport_mode: "websocket_required".to_string(),
        fallback_allowed: false,
        opening: Some(OpeningAuditSnapshot {
            request_line: "GET /codex/responses?source=audit HTTP/1.1".to_string(),
            header_order: vec!["Host".to_string(), "Authorization".to_string()],
            headers: vec![OpeningAuditHeader {
                name: "Sec-WebSocket-Extensions".to_string(),
                value: "identity".to_string(),
            }],
        }),
        payload: Some(PayloadAuditSnapshot {
            top_level_keys: vec!["type".to_string(), "previous_response_id".to_string()],
            body: json!({"type": "response.create"}),
        }),
        error: Some(WebSocketAuditErrorSnapshot {
            classification: "timeout".to_string(),
            message: "timed out".to_string(),
        }),
    };

    let diff = websocket_parity_diff(&current, &reference);

    assert!(diff.differences.iter().any(|difference| {
        difference.path == "opening.header_order"
            && difference.current == json!(["Host", "Connection"])
            && difference.reference == json!(["Host", "Authorization"])
    }));
    assert!(diff
        .differences
        .iter()
        .any(|difference| difference.path == "payload.top_level_keys"));
    assert!(diff
        .differences
        .iter()
        .any(|difference| difference.path == "error.classification"));
}

#[test]
fn codex_websocket_event_to_sse_frame_should_encode_public_events_only() {
    let event = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_ws",
            "object": "response",
            "usage": {
                "input_tokens": 4,
                "output_tokens": 2,
                "total_tokens": 6
            }
        }
    })
    .to_string();

    let frame = websocket_event_to_sse_frame(&event).expect("public event should encode");

    assert_eq!(
        frame,
        format!("event: response.completed\ndata: {event}\n\n")
    );
    assert!(websocket_event_to_sse_frame(r#"{"type":"codex.rate_limits"}"#).is_none());
    assert!(websocket_event_to_sse_frame(r#"{"response":{}}"#).is_none());
    assert!(websocket_event_to_sse_frame("not-json").is_none());
}

#[test]
fn codex_websocket_metadata_turn_state_should_extract_case_insensitive_header() {
    let event = json!({
        "type": "response.metadata",
        "headers": {
            "X-Codex-Turn-State": ["turn-from-metadata"]
        }
    })
    .to_string();

    assert_eq!(
        websocket_metadata_turn_state(&event).as_deref(),
        Some("turn-from-metadata")
    );
}

#[test]
fn codex_websocket_terminal_event_should_match_completed_failed_and_error() {
    assert!(is_terminal_websocket_event("response.completed"));
    assert!(is_terminal_websocket_event("response.failed"));
    assert!(is_terminal_websocket_event("error"));
    assert!(!is_terminal_websocket_event("response.output_text.delta"));
}

#[test]
fn codex_websocket_error_frame_should_classify_rotatable_codes() {
    let frame = json!({
        "type": "response.failed",
        "response": {
            "error": {
                "code": "usage_limit_reached",
                "message": "Rate limit reached. Please try again in 11.054s."
            }
        }
    })
    .to_string();

    let error =
        classify_websocket_error_frame(&frame, WebSocketErrorClassificationProfile::OneShot)
            .expect("rate limit frame should classify");

    assert_eq!(error.status_code, 429);
}

#[test]
fn codex_websocket_error_frame_should_classify_old_gateway_special_codes() {
    let cases = [
        ("quota_exhausted", 402),
        ("token_expired", 401),
        ("account_banned", 403),
        ("previous_response_not_found", 400),
        ("server_is_overloaded", 503),
        ("usage_not_included", 429),
    ];

    for (code, expected_status) in cases {
        let frame = json!({
            "type": "response.failed",
            "response": {
                "error": {
                    "code": code,
                    "message": "classified by old gateway parity"
                }
            }
        })
        .to_string();

        let error =
            classify_websocket_error_frame(&frame, WebSocketErrorClassificationProfile::OneShot)
                .expect("special gateway code should classify");

        assert_eq!(error.status_code, expected_status, "code: {code}");
    }
}

#[test]
fn codex_websocket_error_frame_should_honor_explicit_error_status() {
    let frame = json!({
        "type": "error",
        "status_code": 403,
        "error": {
            "code": "forbidden"
        }
    })
    .to_string();

    let error =
        classify_websocket_error_frame(&frame, WebSocketErrorClassificationProfile::OneShot)
            .expect("explicit status should classify");

    assert_eq!(error.status_code, 403);
}

#[test]
fn codex_websocket_error_frame_should_ignore_success_status_and_unmapped_error() {
    let success_status = json!({
        "type": "error",
        "status": 200,
        "error": {
            "code": "informational"
        }
    })
    .to_string();
    let unmapped_error = json!({
        "type": "error",
        "error": {
            "code": "not_a_rotatable_error"
        }
    })
    .to_string();

    assert!(classify_websocket_error_frame(
        &success_status,
        WebSocketErrorClassificationProfile::OneShot
    )
    .is_none());
    assert!(classify_websocket_error_frame(
        &unmapped_error,
        WebSocketErrorClassificationProfile::OneShot
    )
    .is_none());
}

#[test]
fn codex_websocket_wrapped_error_headers_should_extract_retry_after() {
    let frame = json!({
        "type": "error",
        "headers": {
            "retry-after": ["37"]
        },
        "error": {
            "code": "rate_limit_exceeded"
        }
    })
    .to_string();

    assert_eq!(
        retry_after_seconds_from_wrapped_error_headers(&frame),
        Some(37)
    );
}

#[test]
fn codex_websocket_response_completed_parse_error_should_validate_shape() {
    let frame = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_bad",
            "usage": {
                "input_tokens": "bad",
                "output_tokens": 1,
                "total_tokens": 1
            }
        }
    })
    .to_string();

    let error = websocket_response_completed_parse_error(&frame)
        .expect("invalid response.completed should report a parse error");

    assert!(error.contains("failed to parse ResponseCompleted"));
}

#[test]
fn codex_websocket_response_completed_parse_error_should_reject_missing_id_and_incomplete_usage() {
    let missing_id = json!({
        "type": "response.completed",
        "response": {
            "object": "response",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "total_tokens": 2
            }
        }
    })
    .to_string();
    let incomplete_usage = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_incomplete_usage",
            "object": "response",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1
            }
        }
    })
    .to_string();

    assert!(websocket_response_completed_parse_error(&missing_id)
        .expect("missing id should be rejected")
        .contains("missing field"));
    assert!(websocket_response_completed_parse_error(&incomplete_usage)
        .expect("incomplete usage should be rejected")
        .contains("missing field"));
}

#[test]
fn codex_websocket_event_shape_parse_error_should_detect_field_type_mismatches() {
    let frame = json!({
        "type": "response.output_text.delta",
        "delta": 42
    })
    .to_string();

    assert!(websocket_event_shape_parse_error(&frame));
}

#[test]
fn codex_websocket_response_events_should_detect_missing_required_fields() {
    assert!(websocket_response_created_missing_response(
        r#"{"type":"response.created"}"#
    ));
    assert!(websocket_response_output_text_delta_missing_delta(
        r#"{"type":"response.output_text.delta"}"#
    ));
}

#[test]
fn codex_websocket_output_item_events_should_detect_invalid_item_shape() {
    assert!(websocket_output_item_event_missing_item(
        r#"{"type":"response.output_item.added"}"#
    ));
    assert!(websocket_output_item_event_non_object_item(
        r#"{"type":"response.output_item.done","item":"bad"}"#
    ));
    assert!(websocket_output_item_event_invalid_item_type_tag(
        r#"{"type":"response.output_item.done","item":{}}"#
    ));
}

#[test]
fn codex_websocket_message_output_item_should_validate_required_fields() {
    let frame = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text"
            }]
        }
    })
    .to_string();

    assert!(websocket_message_output_item_event_invalid_required_fields(
        &frame
    ));
}

#[test]
fn codex_websocket_optional_null_fields_should_match_missing_field_behavior() {
    let message = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "id": null,
            "role": "assistant",
            "phase": null,
            "metadata": null,
            "content": [{
                "type": "output_text",
                "text": "hello"
            }]
        }
    })
    .to_string();
    let custom_tool = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "custom_tool_call",
            "id": null,
            "status": null,
            "call_id": "call_1",
            "name": "render",
            "input": "{}"
        }
    })
    .to_string();
    let context_compaction = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "context_compaction",
            "encrypted_content": null
        }
    })
    .to_string();

    assert!(websocket_event_to_sse_frame(&message).is_some());
    assert!(websocket_event_to_sse_frame(&custom_tool).is_some());
    assert!(websocket_event_to_sse_frame(&context_compaction).is_some());
}

#[test]
fn codex_websocket_response_control_events_should_detect_incomplete_and_missing_completed_response()
{
    let incomplete = json!({
        "type": "response.incomplete",
        "response": {
            "incomplete_details": {
                "reason": "max_output_tokens"
            }
        }
    })
    .to_string();

    assert_eq!(
        websocket_incomplete_response_reason(&incomplete).as_deref(),
        Some("max_output_tokens")
    );
    assert!(websocket_response_completed_missing_response(
        r#"{"type":"response.completed"}"#
    ));
}

#[test]
fn codex_websocket_delta_events_should_validate_official_required_fields() {
    assert!(websocket_delta_event_missing_official_required_fields(
        r#"{"type":"response.custom_tool_call_input.delta","delta":"x"}"#
    ));
    assert!(websocket_delta_event_missing_official_required_fields(
        r#"{"type":"response.reasoning_summary_text.delta","delta":"x"}"#
    ));
    assert!(websocket_delta_event_missing_official_required_fields(
        r#"{"type":"response.reasoning_text.delta","delta":"x"}"#
    ));
}

#[test]
fn codex_websocket_output_item_metadata_and_agent_should_validate_required_fields() {
    let invalid_metadata = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "metadata": {
                "turn_id": 7
            }
        }
    })
    .to_string();
    let invalid_agent = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "agent_message",
            "author": "assistant",
            "content": [{
                "type": "input_text"
            }]
        }
    })
    .to_string();

    assert!(websocket_output_item_event_invalid_metadata(
        &invalid_metadata
    ));
    assert!(websocket_agent_message_output_item_event_invalid_required_fields(&invalid_agent));
}

#[test]
fn codex_websocket_output_item_optional_fields_should_reject_old_gateway_edge_cases() {
    let invalid_tool_search_call = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "tool_search_call",
            "call_id": {},
            "status": [],
            "execution": "client",
            "arguments": {}
        }
    })
    .to_string();
    let invalid_custom_tool_call = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "custom_tool_call",
            "id": {},
            "status": [],
            "call_id": "call_1",
            "name": "render",
            "input": "{}"
        }
    })
    .to_string();
    let invalid_custom_tool_output = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "custom_tool_call_output",
            "call_id": "call_1",
            "name": {},
            "output": "ok"
        }
    })
    .to_string();
    let invalid_tool_search_output = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "tool_search_output",
            "call_id": {},
            "status": "completed",
            "execution": "server",
            "tools": []
        }
    })
    .to_string();
    let invalid_image = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "image_generation_call",
            "id": "img_1",
            "status": "completed",
            "revised_prompt": {},
            "result": "base64"
        }
    })
    .to_string();
    let invalid_context_compaction = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "context_compaction",
            "encrypted_content": {}
        }
    })
    .to_string();

    assert!(
        websocket_tool_search_call_output_item_event_invalid_required_fields(
            &invalid_tool_search_call
        )
    );
    assert!(
        websocket_custom_tool_call_output_item_event_invalid_required_fields(
            &invalid_custom_tool_call
        )
    );
    assert!(
        websocket_custom_tool_call_output_payload_item_event_invalid_required_fields(
            &invalid_custom_tool_output
        )
    );
    assert!(
        websocket_tool_search_output_item_event_invalid_required_fields(
            &invalid_tool_search_output
        )
    );
    assert!(
        websocket_image_generation_call_output_item_event_invalid_required_fields(&invalid_image)
    );
    assert!(
        websocket_compaction_output_item_event_invalid_required_fields(&invalid_context_compaction)
    );
}

#[test]
fn codex_websocket_reasoning_and_function_items_should_validate_required_fields() {
    let invalid_reasoning = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "reasoning",
            "summary": [{
                "type": "summary_text"
            }]
        }
    })
    .to_string();
    let invalid_function_call = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "name": "tool",
            "call_id": "call_1"
        }
    })
    .to_string();
    let invalid_function_output = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call_output",
            "call_id": "call_1",
            "output": [{
                "type": "input_text"
            }]
        }
    })
    .to_string();

    assert!(websocket_reasoning_output_item_event_invalid_required_fields(&invalid_reasoning));
    assert!(
        websocket_function_call_output_item_event_invalid_required_fields(&invalid_function_call)
    );
    assert!(
        websocket_function_call_output_payload_item_event_invalid_required_fields(
            &invalid_function_output
        )
    );
}

#[test]
fn codex_websocket_custom_and_tool_search_items_should_validate_required_fields() {
    let invalid_custom_call = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "custom_tool_call",
            "call_id": "call_1",
            "name": "custom"
        }
    })
    .to_string();
    let invalid_custom_output = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "custom_tool_call_output",
            "output": "ok"
        }
    })
    .to_string();
    let invalid_tool_search_call = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "tool_search_call",
            "arguments": {}
        }
    })
    .to_string();
    let invalid_tool_search_output = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "tool_search_output",
            "status": "completed",
            "execution": "remote"
        }
    })
    .to_string();

    assert!(
        websocket_custom_tool_call_output_item_event_invalid_required_fields(&invalid_custom_call)
    );
    assert!(
        websocket_custom_tool_call_output_payload_item_event_invalid_required_fields(
            &invalid_custom_output
        )
    );
    assert!(
        websocket_tool_search_call_output_item_event_invalid_required_fields(
            &invalid_tool_search_call
        )
    );
    assert!(
        websocket_tool_search_output_item_event_invalid_required_fields(
            &invalid_tool_search_output
        )
    );
}

#[test]
fn codex_websocket_local_web_image_and_compaction_items_should_validate_required_fields() {
    let invalid_local_shell = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "local_shell_call",
            "call_id": "call_1",
            "status": "failed",
            "action": {
                "type": "exec",
                "command": "ls"
            }
        }
    })
    .to_string();
    let invalid_web_search = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "web_search_call",
            "status": "completed",
            "action": {}
        }
    })
    .to_string();
    let invalid_image = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "image_generation_call",
            "id": "img_1",
            "status": "completed"
        }
    })
    .to_string();
    let invalid_compaction = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "compaction"
        }
    })
    .to_string();

    assert!(
        websocket_local_shell_call_output_item_event_invalid_required_fields(&invalid_local_shell)
    );
    assert!(
        websocket_web_search_call_output_item_event_invalid_required_fields(&invalid_web_search)
    );
    assert!(
        websocket_image_generation_call_output_item_event_invalid_required_fields(&invalid_image)
    );
    assert!(websocket_compaction_output_item_event_invalid_required_fields(&invalid_compaction));
}

#[test]
fn codex_websocket_local_shell_and_web_search_should_reject_nested_shape_mismatches() {
    let invalid_local_shell = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "local_shell_call",
            "call_id": "call_1",
            "status": "completed",
            "action": {
                "type": "exec",
                "command": "pwd",
                "timeout_ms": "1000",
                "env": {
                    "PATH": {}
                }
            }
        }
    })
    .to_string();
    let invalid_web_search = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "web_search_call",
            "status": "completed",
            "action": {
                "type": "search",
                "query": {},
                "queries": ["valid", {}]
            }
        }
    })
    .to_string();

    assert!(
        websocket_local_shell_call_output_item_event_invalid_required_fields(&invalid_local_shell)
    );
    assert!(
        websocket_web_search_call_output_item_event_invalid_required_fields(&invalid_web_search)
    );
}

#[test]
fn codex_websocket_reasoning_summary_part_should_require_summary_index() {
    assert!(
        websocket_reasoning_summary_part_added_missing_summary_index(
            r#"{"type":"response.reasoning_summary_part.added"}"#
        )
    );
}

#[test]
fn codex_websocket_event_to_sse_frame_should_skip_invalid_json_and_shape_mismatches() {
    let invalid_delta = json!({
        "type": "response.output_text.delta",
        "delta": 42
    })
    .to_string();
    let non_object_item = json!({
        "type": "response.output_item.done",
        "item": 123
    })
    .to_string();

    assert!(websocket_event_to_sse_frame("not-json-from-upstream").is_none());
    assert!(websocket_event_to_sse_frame(&invalid_delta).is_none());
    assert!(websocket_event_to_sse_frame(&non_object_item).is_none());
}

#[test]
fn chat_completion_request_should_translate_to_codex_request() {
    let request = codex_proxy_core::protocol::openai::chat::ChatCompletionRequest {
        model: "gpt-5.5".to_string(),
        stream: true,
        messages: vec![
            codex_proxy_core::protocol::openai::chat::ChatMessage {
                role: "system".to_string(),
                content: Some(json!("be brief")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                function_call: None,
            },
            codex_proxy_core::protocol::openai::chat::ChatMessage {
                role: "user".to_string(),
                content: Some(json!("hello")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                function_call: None,
            },
        ],
        reasoning_effort: Some("medium".to_string()),
        service_tier: Some("auto".to_string()),
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        functions: None,
        response_format: None,
        user: Some(" client-123 ".to_string()),
    };

    let codex = codex_proxy_core::protocol::openai::chat::translate_chat_to_codex(request)
        .expect("chat request should translate");

    assert_eq!(codex.model, "gpt-5.5");
    assert!(codex.force_http_sse);
    assert_eq!(codex.prompt_cache_key.as_deref(), Some("client-123"));
    assert_eq!(codex.client_conversation_id.as_deref(), Some("client-123"));
}

#[test]
fn sse_parser_should_combine_multiline_data_and_metadata() {
    let events = codex_proxy_core::protocol::codex::sse::parse_sse_events(
        "event: message\nid: evt_1\ndata: hello\ndata: world\nretry: 10\n\n",
    )
    .expect("sse should parse");

    assert_eq!(events[0].data, "hello\nworld");
}

#[test]
fn chat_completion_from_codex_sse_should_convert_completed_response() {
    let body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"hello\"}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n",
    );

    let response = codex_proxy_core::protocol::openai::chat::chat_completion_from_codex_sse(
        body, "gpt-5.5", false, None,
    )
    .expect("conversion should succeed")
    .expect("completed response");

    assert_eq!(response["choices"][0]["message"]["content"], "hello");
}

#[test]
fn response_from_codex_sse_should_collect_completed_response_with_deltas() {
    let body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"hello\"}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n",
    );

    let response = response_from_codex_sse(body, None).expect("conversion should succeed");

    assert_eq!(
        response,
        CollectedResponse::Completed(json!({
            "id": "resp_1",
            "object": "response",
            "status": "completed",
            "usage": {
                "input_tokens": 2,
                "output_tokens": 3
            },
            "output": [{
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "hello",
                    "annotations": []
                }]
            }],
            "output_text": "hello"
        }))
    );
}

#[test]
fn openai_response_request_should_translate_to_codex_request() {
    let request = serde_json::from_value::<OpenAiResponsesRequest>(json!({
        "model": "gpt-5.5",
        "input": "hello",
        "previous_response_id": "resp_previous",
        "turnState": "turn_previous",
        "stream": false
    }))
    .expect("responses request should deserialize");

    let codex = translate_response_to_codex(request);

    assert_eq!(codex.model, "gpt-5.5");
    assert_eq!(codex.input, vec![json!("hello")]);
    assert_eq!(codex.previous_response_id.as_deref(), Some("resp_previous"));
    assert_eq!(codex.turn_state.as_deref(), Some("turn_previous"));
    assert!(!codex.use_websocket);
    assert!(!codex.force_http_sse);
}

#[test]
fn openai_response_request_should_default_missing_stream_to_true() {
    let request = serde_json::from_value::<OpenAiResponsesRequest>(json!({
        "model": "gpt-5.5",
        "input": "hello"
    }))
    .expect("responses request should deserialize");

    let codex = translate_response_to_codex(request);

    assert!(codex.stream);
}

#[test]
fn openai_response_request_should_prepare_tuple_schema_before_upstream() {
    let request = serde_json::from_value::<OpenAiResponsesRequest>(json!({
        "model": "gpt-5.5",
        "input": [],
        "stream": false,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "TupleAnswer",
                "schema": {
                    "type": "object",
                    "properties": {
                        "point": {
                            "type": "array",
                            "prefixItems": [
                                {"type": "number"},
                                {"type": "number"}
                            ],
                            "items": false
                        }
                    },
                    "required": ["point"]
                },
                "strict": true
            }
        }
    }))
    .expect("responses request should deserialize");

    let codex = translate_response_to_codex(request);
    let schema = &codex.text.as_ref().unwrap()["format"]["schema"];

    assert_eq!(
        schema["properties"]["point"],
        json!({
            "type": "object",
            "properties": {
                "0": {"type": "number"},
                "1": {"type": "number"}
            },
            "required": ["0", "1"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        codex.tuple_schema.unwrap()["properties"]["point"]["type"],
        "array"
    );
}

#[test]
fn openai_response_request_should_forward_parity_fields_to_codex() {
    let request = serde_json::from_value::<OpenAiResponsesRequest>(json!({
        "model": "gpt-5.5-fast",
        "stream": false,
        "input": [],
        "instructions": "be terse",
        "reasoning": {"effort": "high"},
        "service_tier": "priority",
        "tool_choice": {"type": "function", "function": {"name": "lookup"}},
        "parallel_tool_calls": true,
        "tools": [{"type": "function", "name": "lookup"}],
        "prompt_cache_key": "pcache",
        "include": ["reasoning.encrypted_content"],
        "client_metadata": {"safe": "yes", "drop": 42},
        "turnState": "turn-body",
        "turnMetadata": "meta-body",
        "betaFeatures": "beta-body",
        "includeTimingMetrics": "true",
        "version": "2026-06-12",
        "codexWindowId": "window-body",
        "parentThreadId": "parent-body",
        "use_websocket": false
    }))
    .expect("responses request should deserialize");

    let codex = translate_response_to_codex(request);

    assert_eq!(codex.instructions, "be terse");
    assert_eq!(
        codex.reasoning,
        Some(json!({"effort": "high", "summary": "auto"}))
    );
    assert_eq!(codex.service_tier.as_deref(), Some("priority"));
    assert_eq!(
        codex.tool_choice,
        Some(json!({"type": "function", "function": {"name": "lookup"}}))
    );
    assert_eq!(codex.parallel_tool_calls, Some(true));
    assert_eq!(
        codex.tools,
        Some(vec![json!({"type": "function", "name": "lookup"})])
    );
    assert_eq!(codex.prompt_cache_key.as_deref(), Some("pcache"));
    assert!(codex.explicit_prompt_cache_key);
    assert_eq!(
        codex.include,
        Some(vec!["reasoning.encrypted_content".to_string()])
    );
    assert_eq!(codex.client_metadata, Some(json!({"safe": "yes"})));
    assert_eq!(codex.turn_state.as_deref(), Some("turn-body"));
    assert_eq!(codex.turn_metadata.as_deref(), Some("meta-body"));
    assert_eq!(codex.beta_features.as_deref(), Some("beta-body"));
    assert_eq!(codex.include_timing_metrics.as_deref(), Some("true"));
    assert_eq!(codex.version.as_deref(), Some("2026-06-12"));
    assert_eq!(codex.codex_window_id.as_deref(), Some("window-body"));
    assert_eq!(codex.parent_thread_id.as_deref(), Some("parent-body"));
    assert!(codex.force_http_sse);
}

#[test]
fn codex_responses_model_options_should_apply_suffix_defaults_and_include_reasoning() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5-high-fast", "", Vec::new());
    let parsed = ParsedModelName {
        model_id: "gpt-5.5".to_string(),
        reasoning_effort: Some("high".to_string()),
        service_tier: Some("fast".to_string()),
    };
    let config = ModelConfig {
        default_model: "gpt-5.5".to_string(),
        default_reasoning_effort: Some("medium".to_string()),
        service_tier: Some("flex".to_string()),
        aliases: Default::default(),
    };

    apply_response_model_options(&mut request, &parsed, &config);

    assert_eq!(request.model, "gpt-5.5");
    assert_eq!(
        request.reasoning,
        Some(json!({"summary": "auto", "effort": "high"}))
    );
    assert_eq!(request.service_tier.as_deref(), Some("priority"));
    assert_eq!(
        request.include,
        Some(vec!["reasoning.encrypted_content".to_string()])
    );
}

#[test]
fn codex_responses_model_options_should_preserve_client_include_and_normalize_body_tier() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new());
    request.reasoning = Some(json!({"summary": "detailed"}));
    request.service_tier = Some("fast".to_string());
    request.include = Some(vec!["file_search_call.results".to_string()]);
    let parsed = ParsedModelName {
        model_id: "gpt-5.5".to_string(),
        reasoning_effort: Some("low".to_string()),
        service_tier: Some("flex".to_string()),
    };
    let config = ModelConfig {
        default_model: "gpt-5.5".to_string(),
        default_reasoning_effort: Some("medium".to_string()),
        service_tier: Some("flex".to_string()),
        aliases: Default::default(),
    };

    apply_response_model_options(&mut request, &parsed, &config);

    assert_eq!(
        request.reasoning,
        Some(json!({"summary": "detailed", "effort": "low"}))
    );
    assert_eq!(request.service_tier.as_deref(), Some("priority"));
    assert_eq!(
        request.include,
        Some(vec!["file_search_call.results".to_string()])
    );
}

#[test]
fn openai_compact_request_should_drop_responses_only_fields_and_sanitize_input() {
    let request = serde_json::from_value::<OpenAiResponsesRequest>(json!({
        "model": "gpt-5.5-fast",
        "instructions": "compress the session",
        "input": [
            {"role": "user", "content": "hello"},
            {
                "type": "reasoning",
                "id": "rs_1",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "kept"}],
                "ignored": "drop"
            },
            {"type": "compaction", "encrypted_content": "enc_compact"},
            {"type": "compaction", "id": "drop_missing_encrypted"}
        ],
        "tools": [{"type": "function", "name": "lookup"}],
        "parallel_tool_calls": false,
        "reasoning": {"effort": "high", "summary": "auto", "extra": "drop"},
        "text": {
            "format": {
                "type": "json_schema",
                "name": "Compact",
                "schema": {"type": "object"},
                "strict": true
            }
        },
        "stream": true,
        "store": true,
        "prompt_cache_key": "must_not_forward"
    }))
    .expect("responses request should deserialize");

    let compact = translate_response_to_compact(request);
    let body = serde_json::to_value(&compact).expect("compact request should serialize");

    assert_eq!(body["model"], "gpt-5.5-fast");
    assert_eq!(body["instructions"], "compress the session");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(
        body["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert_eq!(
        body["tools"],
        json!([{"type": "function", "name": "lookup"}])
    );
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert!(body.get("stream").is_none());
    assert!(body.get("store").is_none());
    assert!(body.get("prompt_cache_key").is_none());
    assert_eq!(body["input"].as_array().unwrap().len(), 3);
    assert!(body["input"][1].get("ignored").is_none());
    assert_eq!(body["input"][2]["encrypted_content"], "enc_compact");
}

#[test]
fn openai_streaming_response_with_previous_response_should_require_websocket() {
    let request = serde_json::from_value::<OpenAiResponsesRequest>(json!({
        "model": "gpt-5.5",
        "input": "hello",
        "previous_response_id": "resp_previous",
        "stream": true
    }))
    .expect("responses request should deserialize");

    let codex = translate_response_to_codex(request);

    assert_eq!(codex.previous_response_id.as_deref(), Some("resp_previous"));
    assert!(codex.stream);
    assert!(!codex.force_http_sse);
}

#[test]
fn extract_usage_should_read_codex_usage_shape() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "input_tokens_details": {
                "cached_tokens": 3
            }
        }
    });

    let usage = extract_usage(&body).expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 17,
        }
    );
}

#[test]
fn extract_usage_should_read_image_generation_tokens_separately() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "input_tokens_details": {
                "cached_tokens": 3
            }
        },
        "tool_usage": {
            "image_gen": {
                "input_tokens": 31,
                "output_tokens": 9
            }
        }
    });

    let usage = extract_usage(&body).expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            image_input_tokens: 31,
            image_output_tokens: 9,
            total_tokens: 17,
        }
    );
}

#[test]
fn extract_usage_should_merge_openai_usage_shape() {
    let first = extract_usage(&json!({
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 4,
            "prompt_tokens_details": {
                "cached_tokens": 2
            }
        }
    }))
    .expect("usage should exist");
    let second = TokenUsage {
        input_tokens: 1,
        output_tokens: 2,
        cached_tokens: 0,
        image_input_tokens: 0,
        image_output_tokens: 0,
        total_tokens: 3,
    };

    assert_eq!(
        first.merged(second),
        TokenUsage {
            input_tokens: 9,
            output_tokens: 6,
            cached_tokens: 2,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 15,
        }
    );
}

#[test]
fn extract_sse_usage_should_prefer_completed_response_usage() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n\n",
    );

    let usage = extract_sse_usage(body)
        .expect("usage extraction should succeed")
        .expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 3,
            output_tokens: 5,
            cached_tokens: 1,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 8,
        }
    );
}

#[test]
fn extract_sse_usage_should_read_completed_image_generation_tokens() {
    let body = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":3}},\"tool_usage\":{\"image_gen\":{\"input_tokens\":31,\"output_tokens\":9}}}}\n\n",
    );

    let usage = extract_sse_usage(body)
        .expect("usage extraction should succeed")
        .expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            image_input_tokens: 31,
            image_output_tokens: 9,
            total_tokens: 17,
        }
    );
}

#[test]
fn retry_after_seconds_from_body_should_read_structured_retry_delay() {
    let body = json!({
        "response": {
            "error": {
                "resets_in_seconds": 45
            }
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(45));
}

#[test]
fn retry_after_seconds_from_body_should_parse_rate_limit_message_seconds() {
    let body = json!({
        "response": {
            "error": {
                "code": "rate_limit_exceeded",
                "message": "Rate limit reached. Please try again in 11.054s."
            }
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(12));
}

#[test]
fn retry_after_seconds_from_body_should_parse_rate_limit_message_milliseconds() {
    let body = json!({
        "error": {
            "code": "rate_limit_exceeded",
            "message": "Rate limit reached. Please try again in 28ms."
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(1));
}

#[test]
fn retry_after_seconds_from_body_should_ignore_retry_message_for_other_codes() {
    let body = json!({
        "response": {
            "error": {
                "code": "upstream_transient_error",
                "message": "Try again in 35 seconds."
            }
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), None);
}

#[test]
fn parse_rate_limit_headers_should_extract_primary_secondary_and_review_windows() {
    let headers = vec![
        (
            "x-codex-primary-used-percent".to_string(),
            "100".to_string(),
        ),
        (
            "x-codex-primary-window-minutes".to_string(),
            "5".to_string(),
        ),
        (
            "x-codex-primary-reset-at".to_string(),
            "1893456300".to_string(),
        ),
        (
            "x-codex-secondary-used-percent".to_string(),
            "42.5".to_string(),
        ),
        (
            "x-codex-secondary-window-minutes".to_string(),
            "10080".to_string(),
        ),
        (
            "x-codex-code-review-primary-used-percent".to_string(),
            "80".to_string(),
        ),
        (
            "x-codex-code-review-primary-reset-at".to_string(),
            "1893456600".to_string(),
        ),
    ];

    let parsed = parse_rate_limit_headers(&headers).expect("rate limits should parse");

    assert_eq!(
        parsed.primary,
        Some(RateLimitWindow {
            used_percent: 100.0,
            window_minutes: Some(5),
            reset_at: Some(1_893_456_300),
        })
    );
    assert_eq!(
        parsed.secondary.expect("secondary window").window_minutes,
        Some(10080)
    );
    assert_eq!(
        parsed
            .code_review
            .expect("review window")
            .primary
            .expect("review primary")
            .reset_at,
        Some(1_893_456_600)
    );
}

#[test]
fn parse_rate_limits_event_should_extract_internal_websocket_rate_limits() {
    let event = json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "primary": {
                "used_percent": 99.5,
                "window_minutes": 300,
                "reset_at": 1893456300
            },
            "secondary": {
                "used_percent": 10,
                "window_minutes": 10080,
                "reset_at": 1894056000
            }
        }
    });

    let parsed = parse_rate_limits_event(&event).expect("event should parse");

    assert_eq!(parsed.primary.expect("primary window").used_percent, 99.5);
    assert_eq!(
        parsed.secondary.expect("secondary window").reset_at,
        Some(1_894_056_000)
    );
}

#[test]
fn rate_limit_quota_should_preserve_existing_credits_when_passive_data_lacks_credits() {
    let headers = vec![
        ("x-codex-primary-used-percent".to_string(), "25".to_string()),
        (
            "x-codex-primary-window-minutes".to_string(),
            "5".to_string(),
        ),
        (
            "x-codex-primary-reset-at".to_string(),
            "1893456300".to_string(),
        ),
    ];
    let existing = json!({
        "credits": {
            "has_credits": true,
            "unlimited": false,
            "balance": 12
        }
    });
    let parsed = parse_rate_limit_headers(&headers).expect("rate limits should parse");

    let quota = rate_limit_quota(&parsed, Some("plus"), Some(&existing));

    assert_eq!(quota["plan_type"], "plus");
    assert_eq!(quota["rate_limit"]["remaining_percent"], 75);
    assert_eq!(quota["credits"]["balance"], 12);
}

#[test]
fn cooldown_with_jitter_should_return_positive_duration_within_expected_range() {
    let duration = cooldown_with_jitter(60, 2_000);

    assert!((48..=72).contains(&duration.num_seconds()));
}

#[tokio::test]
async fn refresh_scheduler_should_refresh_before_expiry_and_preserve_refresh_token() {
    use async_trait::async_trait;
    use chrono::{Duration as ChronoDuration, Utc};
    use codex_proxy_core::accounts::model::{Account, AccountStatus};
    use codex_proxy_core::auth::oauth::{
        RefreshFailure, RefreshPolicy, RefreshScheduler, RefreshTrigger, TokenPair,
    };
    use codex_proxy_core::auth::ports::TokenRefresher;

    #[derive(Clone)]
    struct StaticRefreshClient {
        result: Result<TokenPair, RefreshFailure>,
    }

    #[async_trait]
    impl TokenRefresher for StaticRefreshClient {
        async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
            self.result.clone()
        }
    }

    let now = Utc::now();
    let mut account = Account::test("acct_1", AccountStatus::Active);
    account.access_token_expires_at = Some(now + ChronoDuration::seconds(60));
    account.refresh_token = Some("rt_keep".to_string());
    let scheduler = RefreshScheduler::new(
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
        },
        StaticRefreshClient {
            result: Ok(TokenPair {
                access_token: "new-access".to_string(),
                refresh_token: None,
            }),
        },
    );

    let refreshed = scheduler
        .refresh_account_at(&account, RefreshTrigger::BeforeExpiry, now)
        .await
        .expect("refresh should succeed");

    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt_keep"));
    assert_eq!(refreshed.status, AccountStatus::Active);
}

fn assert_substrings_appear_in_order(haystack: &str, needles: &[&str]) {
    let mut cursor = 0;
    for needle in needles {
        let Some(offset) = haystack[cursor..].find(needle) else {
            panic!("expected substring {needle:?} after byte {cursor} in:\n{haystack}");
        };
        cursor += offset + needle.len();
    }
}
