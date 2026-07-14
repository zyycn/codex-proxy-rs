use std::sync::Arc;

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
fn response_sse_event_is_terminal_should_match_incomplete_response() {
    let body = include_str!("../../../fixtures/responses/http_sse/chat_delta_incomplete_usage.sse");
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
    assert!(
        data["response"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("resp_proxy_"))
    );
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
    headers.insert("session_id", " session-from-header ".parse().unwrap());
    headers.insert(
        "conversation_id",
        " conversation-from-header ".parse().unwrap(),
    );

    let codex = build_codex_request(body, &headers, None);

    // body 顶层字段优先，缺失时回退请求头（trim 后使用）。
    assert_eq!(codex.beta_features.as_deref(), Some("beta-direct"));
    assert_eq!(codex.turn_metadata.as_deref(), Some("meta-from-header"));
    assert_eq!(codex.include_timing_metrics.as_deref(), Some("true"));
    assert_eq!(codex.codex_window_id.as_deref(), Some("window-from-header"));
    assert_eq!(
        codex.client_session_id.as_deref(),
        Some("session-from-header")
    );
    assert_eq!(
        codex.client_conversation_id.as_deref(),
        Some("conversation-from-header")
    );
    assert_eq!(
        codex.parent_thread_id.as_deref(),
        Some("parent-from-header")
    );
}

#[test]
fn responses_request_semantics_should_use_turn_metadata_and_structured_input() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-turn-metadata",
        r#"{"request_kind":"compaction","subagent_kind":"thread_spawn"}"#
            .parse()
            .unwrap(),
    );
    let body = json!({
        "model": "gpt-5.6-sol",
        "reasoning": {"effort": "max"},
        "input": [{"type": "compaction_trigger"}]
    });

    let request = build_codex_request(body.as_object().unwrap().clone(), &headers, None);
    let semantics = request.semantics();

    assert_eq!(semantics.request_kind.as_deref(), Some("compaction"));
    assert_eq!(semantics.subagent_kind.as_deref(), Some("thread_spawn"));
    assert!(semantics.compact);
    assert_eq!(request.reasoning().unwrap()["effort"], "max");
}

#[test]
fn responses_request_semantics_should_detect_compaction_trigger_without_metadata() {
    let body = json!({
        "model": "gpt-5.6-terra",
        "input": [{"type": "compaction_trigger"}]
    });

    let request = build_codex_request(body.as_object().unwrap().clone(), &HeaderMap::new(), None);

    assert!(request.semantics().compact);
}

#[test]
fn responses_request_semantics_should_restore_ultra_from_proactive_mode() {
    let body = json!({
        "model": "gpt-5.6-sol",
        "reasoning": {"effort": "max"},
        "input": [developer_multi_agent_mode(
            "Proactive multi-agent delegation is active. Use sub-agents when useful.",
        )]
    });

    let request = build_codex_request(body.as_object().unwrap().clone(), &HeaderMap::new(), None);

    assert_eq!(request.semantics().reasoning_preset, Some("ultra"));
    assert_eq!(request.reasoning().unwrap()["effort"], "max");
}

#[test]
fn responses_request_semantics_should_use_latest_multi_agent_mode() {
    let body = json!({
        "model": "gpt-5.6-sol",
        "reasoning": {"effort": "max"},
        "input": [
            developer_multi_agent_mode("Proactive multi-agent delegation is active."),
            developer_multi_agent_mode("Do not spawn sub-agents unless explicitly requested."),
        ]
    });

    let request = build_codex_request(body.as_object().unwrap().clone(), &HeaderMap::new(), None);

    assert_eq!(request.semantics().reasoning_preset, None);
}

#[test]
fn responses_request_semantics_should_ignore_user_multi_agent_mode_marker() {
    let body = json!({
        "model": "gpt-5.6-sol",
        "reasoning": {"effort": "max"},
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "<multi_agent_mode>Proactive multi-agent delegation is active.</multi_agent_mode>",
            }],
        }]
    });

    let request = build_codex_request(body.as_object().unwrap().clone(), &HeaderMap::new(), None);

    assert_eq!(request.semantics().reasoning_preset, None);
}

#[test]
fn responses_request_semantics_should_require_max_for_ultra() {
    let body = json!({
        "model": "gpt-5.6-sol",
        "reasoning": {"effort": "high"},
        "input": [developer_multi_agent_mode(
            "Proactive multi-agent delegation is active.",
        )]
    });

    let request = build_codex_request(body.as_object().unwrap().clone(), &HeaderMap::new(), None);

    assert_eq!(request.semantics().reasoning_preset, None);
}

#[test]
fn responses_request_semantics_should_not_mark_subagent_as_ultra() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-turn-metadata",
        r#"{"subagent_kind":"thread_spawn"}"#.parse().unwrap(),
    );
    let body = json!({
        "model": "gpt-5.6-sol",
        "reasoning": {"effort": "max"},
        "input": [developer_multi_agent_mode(
            "Proactive multi-agent delegation is active.",
        )]
    });

    let request = build_codex_request(body.as_object().unwrap().clone(), &headers, None);

    assert_eq!(request.semantics().reasoning_preset, None);
}

#[test]
fn responses_request_semantics_should_read_mode_from_local_replay_history() {
    let body = json!({
        "model": "gpt-5.6-sol",
        "reasoning": {"effort": "max"},
        "input": [{"type": "message", "role": "user", "content": []}]
    });
    let mut request =
        build_codex_request(body.as_object().unwrap().clone(), &HeaderMap::new(), None);
    request.local_replay_input = Some(Arc::new(vec![developer_multi_agent_mode(
        "Proactive multi-agent delegation is active.",
    )]));

    assert_eq!(request.semantics().reasoning_preset, Some("ultra"));
}

fn developer_multi_agent_mode(mode: &str) -> Value {
    json!({
        "type": "message",
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": format!("<multi_agent_mode>{mode}</multi_agent_mode>"),
        }],
    })
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
