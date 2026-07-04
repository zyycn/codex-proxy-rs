use super::*;

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
            "input",
            "store",
            "stream",
            "previous_response_id",
            "reasoning",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "text",
            "service_tier",
            "prompt_cache_key",
            "include",
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
fn codex_websocket_response_create_payload_text_should_preserve_canonical_field_order() {
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
        codex_proxy_rs::upstream::protocol::websocket::websocket_response_create_payload_text(
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
            "\"store\":false",
            "\"stream\":true",
            "\"tool_choice\":\"auto\"",
            "\"parallel_tool_calls\":true",
            "\"prompt_cache_key\":\"session-1\"",
            "\"client_metadata\":",
        ],
    );
}

#[test]
fn codex_websocket_response_create_payload_text_should_include_empty_instructions() {
    let request = CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new());

    let payload =
        codex_proxy_rs::upstream::protocol::websocket::websocket_response_create_payload_text(
            &request,
        )
        .expect("payload should serialize");
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let snapshot = websocket_payload_audit_snapshot(&request);

    assert_eq!(value["instructions"], "");
    assert_eq!(snapshot.top_level_keys[2], "instructions");
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
    request.force_http_sse = false;
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

    let error = classify_websocket_error_frame(&frame).expect("rate limit frame should classify");

    assert_eq!(error.status_code, 429);
}

#[test]
fn codex_websocket_error_frame_should_classify_ts_aligned_upstream_error_codes() {
    let cases = [
        ("quota_exhausted", 402),
        ("payment_required", 402),
        ("unauthorized", 401),
        ("token_invalid", 401),
        ("token_expired", 401),
        ("account_deactivated", 401),
        ("forbidden", 403),
        ("account_banned", 403),
        ("banned", 403),
        ("previous_response_not_found", 400),
    ];

    for (code, expected_status) in cases {
        let frame = json!({
            "type": "response.failed",
            "response": {
                "error": {
                    "code": code,
                    "message": "classified by upstream error mapping"
                }
            }
        })
        .to_string();

        let error = classify_websocket_error_frame(&frame)
            .expect("TS-aligned upstream code should classify");

        assert_eq!(error.status_code, expected_status, "code: {code}");
    }
}

#[test]
fn codex_websocket_error_frame_should_ignore_legacy_extension_codes() {
    let cases = [
        "quota_exceeded",
        "insufficient_quota",
        "usage_not_included",
        "invalid_plan",
        "banned_unknown_charge",
        "context_length_exceeded",
        "invalid_prompt",
        "cyber_policy",
        "invalid_request",
        "invalid_encrypted_content",
        "no_tool_output_found_for_function_call",
        "server_is_overloaded",
        "slow_down",
        "temporarily_unavailable",
        "over_capacity",
        "server_error",
        "upstream_error",
        "rate_limited",
    ];

    for code in cases {
        let frame = json!({
            "type": "response.failed",
            "response": {
                "error": {
                    "code": code,
                    "message": "not part of the TS WebSocket rotation allowlist"
                }
            }
        })
        .to_string();

        assert!(
            classify_websocket_error_frame(&frame).is_none(),
            "code should pass through: {code}"
        );
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

    let error = classify_websocket_error_frame(&frame).expect("explicit status should classify");

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

    assert!(classify_websocket_error_frame(&success_status).is_none());
    assert!(classify_websocket_error_frame(&unmapped_error).is_none());
}

#[test]
fn codex_websocket_error_frame_should_ignore_unmapped_response_failed() {
    let frame = json!({
        "type": "response.failed",
        "response": {
            "error": {
                "code": "model_refusal",
                "message": "The model refused the request"
            }
        }
    })
    .to_string();

    let classified = classify_websocket_error_frame(&frame);

    assert!(classified.is_none());
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

    let frame = json!({
        "type": "error",
        "error": {
            "code": "rate_limit_exceeded",
            "retry_after_seconds": 19
        }
    })
    .to_string();

    assert_eq!(
        retry_after_seconds_from_wrapped_error_headers(&frame),
        Some(19)
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
fn codex_websocket_output_item_optional_fields_should_reject_upstream_error_edge_cases() {
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
