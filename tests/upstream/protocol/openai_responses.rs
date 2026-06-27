use super::*;

#[test]
fn codex_responses_request_should_enable_http_sse_defaults() {
    let request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);

    assert!(request.stream);
}

#[test]
fn codex_responses_transport_should_use_http_sse_by_default_without_history() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);
    request.force_http_sse = false;

    assert_eq!(transport_for_request(&request), CodexTransport::HttpSse);
    assert!(http_sse_fallback_allowed(&request));
}

#[test]
fn codex_responses_transport_should_prefer_websocket_when_requested_without_history() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);
    request.use_websocket = true;

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketPreferred
    );
    assert!(http_sse_fallback_allowed(&request));
}

#[test]
fn codex_responses_transport_should_require_websocket_for_previous_response_id() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);
    request.force_http_sse = false;
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
fn response_failed_sse_event_should_encode_openai_failure_shape() {
    let frame = response_failed_sse_event("server_error", "stream_disconnected", "closed early");

    let events = parse_sse_events(&frame).expect("frame should be valid SSE");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.event.as_deref(), Some("response.failed"));
    let data =
        serde_json::from_str::<serde_json::Value>(&event.data).expect("event data should be JSON");

    assert_eq!(data["type"], "response.failed");
    assert_eq!(data["response"]["status"], "failed");
    assert_eq!(data["response"]["error"]["type"], "server_error");
    assert_eq!(data["response"]["error"]["code"], "stream_disconnected");
    assert_eq!(data["response"]["error"]["message"], "closed early");
    assert_eq!(data["error"], data["response"]["error"]);
    assert!(data["response"]["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("resp_proxy_")));
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
    assert_eq!(
        codex.input,
        vec![json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "hello"
            }]
        })]
    );
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
fn openai_response_request_should_fallback_context_fields_to_client_metadata() {
    let request = serde_json::from_value::<OpenAiResponsesRequest>(json!({
        "model": "gpt-5.5",
        "stream": false,
        "input": [],
        "client_metadata": {
            "x-codex-turn-metadata": " meta-from-metadata ",
            "x-codex-beta-features": " beta-from-metadata ",
            "x-responsesapi-include-timing-metrics": " true ",
            "x-codex-window-id": " window-from-metadata ",
            "x-codex-parent-thread-id": " parent-from-metadata "
        },
        "betaFeatures": "beta-direct"
    }))
    .expect("responses request should deserialize");

    let codex = translate_response_to_codex(request);

    assert_eq!(codex.turn_metadata.as_deref(), Some("meta-from-metadata"));
    assert_eq!(codex.beta_features.as_deref(), Some("beta-direct"));
    assert_eq!(codex.include_timing_metrics.as_deref(), Some("true"));
    assert_eq!(
        codex.codex_window_id.as_deref(),
        Some("window-from-metadata")
    );
    assert_eq!(
        codex.parent_thread_id.as_deref(),
        Some("parent-from-metadata")
    );
}

#[test]
fn codex_responses_model_options_should_apply_suffix_defaults_and_include_reasoning() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5-high-fast", "", Vec::new());
    let parsed = ParsedModelName {
        model_id: "gpt-5.5".to_string(),
        reasoning_effort: Some("high".to_string()),
        service_tier: Some("fast".to_string()),
    };
    apply_response_model_options(&mut request, &parsed);

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
    apply_response_model_options(&mut request, &parsed);

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
