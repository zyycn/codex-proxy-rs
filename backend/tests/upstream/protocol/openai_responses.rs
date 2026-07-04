use serde_json::Value;

use super::*;

#[test]
fn codex_responses_request_should_enable_http_sse_defaults() {
    let request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);

    assert!(request.stream());
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
    request.set_previous_response_id(Some("resp_previous".to_string()));

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketRequired
    );
    assert!(!http_sse_fallback_allowed(&request));
}

#[test]
fn codex_responses_transport_should_allow_forced_http_sse() {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", vec![json!({})]);
    request.set_previous_response_id(Some("resp_previous".to_string()));
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
    let body = include_str!("../../fixtures/responses/http_sse/text_delta_completed_usage.sse");

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
fn response_from_codex_sse_should_collect_incomplete_response() {
    let body = include_str!("../../fixtures/responses/http_sse/chat_delta_incomplete_usage.sse");

    let response = response_from_codex_sse(body, None).expect("conversion should succeed");

    assert_eq!(
        response,
        CollectedResponse::Completed(json!({
            "id": "resp_incomplete",
            "object": "response",
            "status": "incomplete",
            "incomplete_details": {
                "reason": "max_output_tokens"
            },
            "usage": {
                "input_tokens": 2,
                "output_tokens": 3
            },
            "output": [{
                "type": "message",
                "status": "incomplete",
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
fn response_sse_event_is_terminal_should_match_incomplete_response() {
    let body = include_str!("../../fixtures/responses/http_sse/chat_delta_incomplete_usage.sse");
    let events = parse_sse_events(body).expect("sse should parse");

    assert!(events.iter().any(response_sse_event_is_terminal));
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

fn responses_body(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        _ => panic!("responses body fixture must be a JSON object"),
    }
}

#[test]
fn build_codex_request_should_pass_through_input_and_preserve_context_fields() {
    let codex = build_codex_request(
        responses_body(json!({
            "model": "gpt-5.5",
            "input": "hello",
            "previous_response_id": "resp_previous",
            "turnState": "turn_previous",
            "stream": false
        })),
        &HeaderMap::new(),
        None,
    );

    assert_eq!(codex.model(), "gpt-5.5");
    // input 原样透传：客户端发字符串就保持字符串，不改写成 message 数组。
    assert_eq!(codex.body().get("input"), Some(&json!("hello")));
    assert_eq!(codex.previous_response_id(), Some("resp_previous"));
    // 透明代理：turnState 既提取到代理控制状态，又原样保留在 body 中透传上游。
    assert_eq!(codex.turn_state.as_deref(), Some("turn_previous"));
    assert_eq!(
        codex.body().get("turnState").and_then(Value::as_str),
        Some("turn_previous")
    );
    assert!(!codex.use_websocket);
    assert!(!codex.force_http_sse);
}

#[test]
fn build_codex_request_should_pass_through_array_input_items_verbatim() {
    let input = json!([
        {
            "type": "reasoning",
            "id": "rs_1",
            "status": "completed",
            "summary": [
                {"type": "summary_text", "text": "valid summary"},
                {"type": "ignored", "text": "preserve"}
            ],
            "encrypted_content": "enc_reasoning",
            "content": [
                {"type": "reasoning_text", "text": "raw reasoning"},
                {"type": "ignored", "text": "preserve"}
            ],
            "extra": "preserve"
        },
        {
            "type": "reasoning",
            "id": "",
            "summary": [{"type": "summary_text", "text": "preserve invalid item"}]
        },
        {"type": "compaction", "id": "cmp_missing_encrypted", "extra": "preserve"},
        "hello"
    ]);
    let body = responses_body(json!({
        "model": "gpt-5.5",
        "input": input,
        "stream": false
    }));

    let codex = build_codex_request(body, &HeaderMap::new(), None);

    assert_eq!(
        codex.input(),
        &[
            json!({
                "type": "reasoning",
                "id": "rs_1",
                "status": "completed",
                "summary": [
                    {"type": "summary_text", "text": "valid summary"},
                    {"type": "ignored", "text": "preserve"}
                ],
                "encrypted_content": "enc_reasoning",
                "content": [
                    {"type": "reasoning_text", "text": "raw reasoning"},
                    {"type": "ignored", "text": "preserve"}
                ],
                "extra": "preserve"
            }),
            json!({
                "type": "reasoning",
                "id": "",
                "summary": [{"type": "summary_text", "text": "preserve invalid item"}]
            }),
            json!({"type": "compaction", "id": "cmp_missing_encrypted", "extra": "preserve"}),
            json!("hello")
        ]
    );
}

#[test]
fn openai_response_request_should_default_missing_stream_to_true() {
    let body = responses_body(json!({
        "model": "gpt-5.5",
        "input": "hello"
    }));

    let codex = build_codex_request(body, &HeaderMap::new(), None);

    assert!(codex.stream());
}

#[test]
fn openai_response_request_should_preserve_text_schema_verbatim() {
    // 透明代理：`text` 原样透传，不再重写 tuple schema（prefixItems → object），
    // 也不再填充 `tuple_schema`（响应侧回转换已不适用）。
    let text = json!({
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
    });
    let mut body = responses_body(json!({
        "model": "gpt-5.5",
        "input": [],
        "stream": false
    }));
    body.insert("text".to_string(), text.clone());

    let codex = build_codex_request(body, &HeaderMap::new(), None);

    assert_eq!(codex.body().get("text"), Some(&text));
    assert!(codex.tuple_schema.is_none());
}

#[test]
fn build_codex_request_should_pass_through_body_fields_verbatim() {
    let body = json!({
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
    })
    .as_object()
    .cloned()
    .unwrap();

    let codex = build_codex_request(body, &HeaderMap::new(), None);
    let upstream = serde_json::to_value(&codex).expect("upstream body should serialize");

    // 透明代理：语义字段原样透传，不注入默认值、不过滤 client_metadata 非字符串项。
    assert_eq!(upstream["instructions"], "be terse");
    assert_eq!(upstream["reasoning"], json!({"effort": "high"}));
    assert_eq!(upstream["service_tier"], "priority");
    assert_eq!(
        upstream["tool_choice"],
        json!({"type": "function", "function": {"name": "lookup"}})
    );
    assert_eq!(upstream["parallel_tool_calls"], true);
    assert_eq!(
        upstream["tools"],
        json!([{"type": "function", "name": "lookup"}])
    );
    assert_eq!(upstream["prompt_cache_key"], "pcache");
    assert_eq!(upstream["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(
        upstream["client_metadata"],
        json!({"safe": "yes", "drop": 42})
    );
    // transport-only 字段不进上游 body。
    assert!(upstream.get("use_websocket").is_none());

    assert!(codex.explicit_prompt_cache_key);
    // 上下文透传字段提取到代理控制状态用于加请求头（body 中同时保留原值）。
    assert_eq!(codex.turn_state.as_deref(), Some("turn-body"));
    assert_eq!(codex.turn_metadata.as_deref(), Some("meta-body"));
    assert_eq!(codex.beta_features.as_deref(), Some("beta-body"));
    assert_eq!(codex.include_timing_metrics.as_deref(), Some("true"));
    assert_eq!(codex.version.as_deref(), Some("2026-06-12"));
    assert_eq!(codex.codex_window_id.as_deref(), Some("window-body"));
    assert_eq!(codex.parent_thread_id.as_deref(), Some("parent-body"));
    assert_eq!(upstream["turnState"], "turn-body");
    assert!(codex.force_http_sse);
}

#[test]
fn build_codex_request_should_prefer_body_context_fields_then_fall_back_to_headers() {
    let body = json!({
        "model": "gpt-5.5",
        "stream": false,
        "input": [],
        "betaFeatures": "beta-direct"
    })
    .as_object()
    .cloned()
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-turn-metadata",
        " meta-from-header ".parse().unwrap(),
    );
    headers.insert(
        "x-codex-beta-features",
        " beta-from-header ".parse().unwrap(),
    );
    headers.insert(
        "x-responsesapi-include-timing-metrics",
        " true ".parse().unwrap(),
    );
    headers.insert("x-codex-window-id", " window-from-header ".parse().unwrap());
    headers.insert(
        "x-codex-parent-thread-id",
        " parent-from-header ".parse().unwrap(),
    );

    let codex = build_codex_request(body, &headers, None);

    // body 顶层字段优先，缺失时回退请求头（trim 后使用）。
    assert_eq!(codex.beta_features.as_deref(), Some("beta-direct"));
    assert_eq!(codex.turn_metadata.as_deref(), Some("meta-from-header"));
    assert_eq!(codex.include_timing_metrics.as_deref(), Some("true"));
    assert_eq!(codex.codex_window_id.as_deref(), Some("window-from-header"));
    assert_eq!(
        codex.parent_thread_id.as_deref(),
        Some("parent-from-header")
    );
}

#[test]
fn openai_compact_request_should_strip_only_stream_and_pass_through_rest() {
    // compact 端点仅支持非流式响应，故只剥离 transport 控制字段 `stream`；
    // 其余字段（store/prompt_cache_key/previous_response_id/include/client_metadata、
    // reasoning 未知键、text、input item）作为业务语义原样透传上游。
    let body = json!({
        "model": "gpt-5.5-fast",
        "instructions": "compress the session",
        "input": [
            {"role": "user", "content": "hello"},
            {
                "type": "reasoning",
                "id": "rs_1",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "kept"}],
                "ignored": "keep"
            },
            {"type": "compaction", "encrypted_content": "enc_compact"},
            {"type": "compaction", "id": "keep_missing_encrypted"}
        ],
        "tools": [{"type": "function", "name": "lookup"}],
        "parallel_tool_calls": false,
        "reasoning": {"effort": "high", "summary": "auto", "extra": "keep"},
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
        "prompt_cache_key": "keep",
        "previous_response_id": "resp_previous",
        "include": ["reasoning.encrypted_content"],
        "client_metadata": {"keep": "yes"}
    })
    .as_object()
    .unwrap()
    .clone();

    let compact = build_compact_request(body, &HeaderMap::new());
    let body = serde_json::to_value(&compact).expect("compact request should serialize");

    assert_eq!(body["model"], "gpt-5.5-fast");
    assert_eq!(body["instructions"], "compress the session");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(
        body["reasoning"],
        json!({"effort": "high", "summary": "auto", "extra": "keep"})
    );
    assert_eq!(
        body["tools"],
        json!([{"type": "function", "name": "lookup"}])
    );
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    // 仅 stream 被剥离。
    assert!(body.get("stream").is_none());
    // 其余字段原样透传。
    assert_eq!(body["store"], true);
    assert_eq!(body["prompt_cache_key"], "keep");
    assert_eq!(body["previous_response_id"], "resp_previous");
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["client_metadata"], json!({"keep": "yes"}));
    assert_eq!(body["input"].as_array().unwrap().len(), 4);
    assert_eq!(body["input"][1]["ignored"], "keep");
    assert_eq!(body["input"][2]["encrypted_content"], "enc_compact");
    assert_eq!(body["input"][3]["id"], "keep_missing_encrypted");
}

#[test]
fn openai_streaming_response_with_previous_response_should_require_websocket() {
    let body = json!({
        "model": "gpt-5.5",
        "input": "hello",
        "previous_response_id": "resp_previous",
        "stream": true
    });

    let codex = build_codex_request(body.as_object().unwrap().clone(), &HeaderMap::new(), None);

    assert_eq!(codex.previous_response_id(), Some("resp_previous"));
    assert!(codex.stream());
    assert!(!codex.force_http_sse);
    assert_eq!(
        transport_for_request(&codex),
        CodexTransport::WebSocketRequired
    );
}
