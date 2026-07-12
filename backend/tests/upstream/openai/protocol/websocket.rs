use super::*;

#[test]
fn codex_websocket_payload_audit_snapshot_should_redact_user_content() {
    let mut request = CodexResponsesRequest::from_body(
        json!({
            "model": "gpt-5.5",
            "instructions": "private instructions",
            "input": [{
                "role": "user",
                "content": "private prompt",
            }],
            "previous_response_id": "resp_secret",
            "service_tier": "flex",
        })
        .as_object()
        .expect("request fixture should be an object")
        .clone(),
    );
    request.set_prompt_cache_key(Some("cache-secret".to_string()));
    request.set_client_metadata(Some(json!({
        "thread_id": "thread-secret",
        "safe": "value",
    })));

    let snapshot = websocket_payload_audit_snapshot(&request);

    // 透明代理：payload key 顺序即 `type` 前置到原始 body 的插入顺序，
    // 不再重排为固定 canonical 顺序，也不注入 tool_choice/parallel_tool_calls 等默认字段。
    assert_eq!(
        snapshot.top_level_keys,
        vec![
            "type",
            "model",
            "instructions",
            "input",
            "previous_response_id",
            "service_tier",
            "prompt_cache_key",
            "client_metadata",
        ]
    );
    assert_eq!(snapshot.body["type"], "response.create");
    assert_eq!(snapshot.body["model"], "gpt-5.5");
    assert!(snapshot.body.get("stream").is_none());
    assert!(snapshot.body.get("store").is_none());
    assert_eq!(snapshot.body["instructions"], "<redacted>");
    assert_eq!(snapshot.body["input"], "<redacted>");
    assert_eq!(snapshot.body["previous_response_id"], "<redacted>");
    assert_eq!(snapshot.body["prompt_cache_key"], "<redacted>");
    assert_eq!(snapshot.body["client_metadata"], "<redacted>");
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
    request.set_prompt_cache_key(Some("session-1".to_string()));
    request.set_client_metadata(Some(json!({
        "thread_id": "capture-thread-secret",
        "safe": "capture",
    })));

    let payload =
        codex_proxy_rs::upstream::openai::protocol::websocket::websocket_response_create_payload_text(
            &request,
        )
        .expect("payload should serialize");

    // 透明代理：payload 即 `type` 前置到原始 body 的插入顺序，字段原样透传，
    // 不注入 tool_choice/parallel_tool_calls 默认值。
    assert_substrings_appear_in_order(
        &payload,
        &[
            "\"type\":\"response.create\"",
            "\"model\":\"gpt-5.5\"",
            "\"instructions\":\"private capture instructions\"",
            "\"input\":",
            "\"prompt_cache_key\":\"session-1\"",
            "\"client_metadata\":",
        ],
    );
}

#[test]
fn codex_websocket_response_create_payload_text_should_include_empty_instructions() {
    let request = CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new());

    let payload =
        codex_proxy_rs::upstream::openai::protocol::websocket::websocket_response_create_payload_text(
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
    request.set_previous_response_id(Some("resp_secret".to_string()));
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
    assert!(is_terminal_websocket_event("response.incomplete"));
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
        ("invalid_encrypted_content", 400),
        ("missing_tool_output", 400),
        ("no_tool_output", 400),
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
fn codex_websocket_response_completed_id_should_validate_shape() {
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

    let error = websocket_response_completed_id(&frame)
        .expect_err("invalid response.completed should report a parse error");

    assert!(error.contains("failed to parse ResponseCompleted"));
}

#[test]
fn codex_websocket_response_completed_id_should_reject_missing_id_and_incomplete_usage() {
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

    assert!(
        websocket_response_completed_id(&missing_id)
            .expect_err("missing id should be rejected")
            .contains("missing field")
    );
    assert!(
        websocket_response_completed_id(&incomplete_usage)
            .expect_err("incomplete usage should be rejected")
            .contains("missing field")
    );
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
fn codex_websocket_response_control_events_should_recognize_incomplete_as_terminal() {
    assert!(is_terminal_websocket_event("response.incomplete"));
}

#[test]
fn codex_websocket_event_to_sse_frame_should_forward_typed_events_without_schema_filtering() {
    // 透明代理：WS 输出侧不再按官方 schema 校验丢弃事件。只有无法确定 type 的帧
    // （非 JSON / 缺 type）和内部事件（codex.rate_limits）会被丢弃，其余原样转发。
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

    // 无 type / 非 JSON：无法编码，丢弃。
    assert!(websocket_event_to_sse_frame("not-json-from-upstream").is_none());
    // 内部事件：transport 剥离，丢弃。
    assert!(websocket_event_to_sse_frame(r#"{"type":"codex.rate_limits"}"#).is_none());
    // 有 type 的畸形事件：原样透传，不再做 schema 过滤。
    let invalid_delta_frame =
        websocket_event_to_sse_frame(&invalid_delta).expect("typed event should be forwarded");
    assert!(invalid_delta_frame.contains("event: response.output_text.delta"));
    assert!(invalid_delta_frame.contains(&invalid_delta));
    let non_object_item_frame =
        websocket_event_to_sse_frame(&non_object_item).expect("typed event should be forwarded");
    assert!(non_object_item_frame.contains("event: response.output_item.done"));
    assert!(non_object_item_frame.contains(&non_object_item));
}
