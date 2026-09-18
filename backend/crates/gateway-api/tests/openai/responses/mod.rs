mod http;
mod request;
mod websocket;

use axum::http::{HeaderMap, HeaderValue};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use gateway_api::openai::responses::{
    ContinuationIntent, DecodedResponsesRequest, OpenAiRequestHeaders, OpenAiResponsesEncoder,
    RequestDecodeError, ResponseCreateFrameError, ResponseEncodeError, decode_request_with_headers,
    decode_response_create_with_context,
};
use gateway_core::{
    event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta},
    operation::{Feature, Operation},
    routing::{PublicModelId, RoutingContext},
};
use gateway_protocol::openai::sse::parse_sse_events;
use serde_json::{Value, json};

fn decode_request(body: &[u8]) -> Result<DecodedResponsesRequest, RequestDecodeError> {
    decode_request_with_headers(body, &HeaderMap::new(), 64 * 1024 * 1024)
}

fn decode_response_create(
    payload: &str,
) -> Result<DecodedResponsesRequest, ResponseCreateFrameError> {
    decode_response_create_with_context(payload, &OpenAiRequestHeaders::default())
}

fn generate_request(body: Value) -> DecodedResponsesRequest {
    decode_request(body.to_string().as_bytes()).expect("test request should decode")
}

fn generate_operation(
    decoded: &DecodedResponsesRequest,
) -> &gateway_core::operation::GenerateRequest {
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses decoder must produce Generate")
    };
    request
}

fn openai_wire_body(decoded: &DecodedResponsesRequest) -> &serde_json::Map<String, Value> {
    generate_operation(decoded).protocol_payload().body()
}

fn openai_protocol_context(decoded: &DecodedResponsesRequest) -> &serde_json::Map<String, Value> {
    generate_operation(decoded).protocol_payload().context()
}

fn openai_wire_event(canonical: Vec<GatewayEvent>, event_type: &str, data: Value) -> ProviderEvent {
    let wire = ProtocolWireEvent::json("openai", Some(event_type.to_owned()), data)
        .expect("valid OpenAI wire event");
    if canonical.is_empty() {
        ProviderEvent::wire(wire)
    } else {
        ProviderEvent::canonical_with_wire(canonical, wire)
    }
}

#[test]
fn decoder_should_preserve_the_openai_body_and_only_derive_stable_routing_facts() {
    let request_body = json!({
        "model": "smart-code",
        "input": [{
            "role": "user",
            "content": [
                {"type": "input_text", "text": "describe"},
                {"type": "input_image", "image_url": "https://example.invalid/image.png"}
            ]
        }],
        "tools": [{"type": "function", "name": "weather", "parameters": {"type": "object"}}],
        "text": {"format": {"type": "json_schema", "name": "weather", "schema": {"type": "object"}}},
        "reasoning": {"effort": "future-value"},
        "max_output_tokens": 512,
        "stream": false,
        "store": true,
        "background": true,
        "future_official_field": {"keep": true}
    });
    let decoded = generate_request(request_body.clone());
    let requirements = decoded.operation().capability_requirements();

    assert_eq!(
        Value::Object(openai_wire_body(&decoded).clone()),
        request_body
    );
    assert_eq!(
        openai_wire_body(&decoded)
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "model",
            "input",
            "tools",
            "text",
            "reasoning",
            "max_output_tokens",
            "stream",
            "store",
            "background",
            "future_official_field"
        ]
    );
    assert_eq!(requirements.requested_output_tokens(), Some(512));
    assert!(
        requirements
            .features()
            .is_superset(&std::collections::BTreeSet::from([
                Feature::Tools,
                Feature::Vision,
                Feature::Reasoning,
                Feature::JsonSchema,
            ]))
    );
    assert!(!decoded.metadata().stream());
    assert!(decoded.metadata().store());
}

#[test]
fn decoder_should_preserve_opaque_client_model_values() {
    for model in [
        format!("future-{}", "x".repeat(512)),
        "future\0model".to_owned(),
        "  future-model  ".to_owned(),
    ] {
        let decoded = generate_request(json!({
            "model": model.clone(),
            "input": "hello"
        }));

        assert_eq!(decoded.metadata().requested_model(), model);
        assert_eq!(
            openai_wire_body(&decoded)
                .get("model")
                .and_then(Value::as_str),
            Some(model.as_str())
        );
    }
}

#[test]
fn decoder_should_preserve_compaction_trigger_for_the_openai_provider() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": [
            {"type": "message", "role": "user", "content": "history"},
            {"type": "compaction_trigger"}
        ]
    }));

    assert_eq!(
        Value::Object(openai_wire_body(&decoded).clone()).pointer("/input/1/type"),
        Some(&json!("compaction_trigger"))
    );
    assert!(matches!(decoded.operation(), Operation::Generate(_)));
}

#[test]
fn decoder_should_keep_transport_override_out_of_the_openai_wire_body() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": "hello",
        "use_websocket": true
    }));

    assert!(!openai_wire_body(&decoded).contains_key("use_websocket"));
    assert_eq!(
        openai_protocol_context(&decoded).get("use_websocket"),
        Some(&json!(true))
    );
}

#[test]
fn decoder_should_preserve_unrecognized_use_websocket_values() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": "hello",
        "use_websocket": {"future": "transport-mode"}
    }));

    assert_eq!(
        openai_wire_body(&decoded).get("use_websocket"),
        Some(&json!({"future": "transport-mode"}))
    );
    assert!(!openai_protocol_context(&decoded).contains_key("use_websocket"));
}

#[test]
fn decoder_should_preserve_connection_metadata_outside_the_openai_wire_body() {
    let mut headers = HeaderMap::new();
    headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-state"));
    headers.insert(
        "x-codex-turn-metadata",
        HeaderValue::from_static("{\"kind\":\"review\"}"),
    );
    headers.insert("conversation-id", HeaderValue::from_static("conversation"));
    let body = json!({
        "model": "smart-code",
        "input": "hello",
        "client_metadata": {
            "x-codex-turn-metadata": "{\"kind\":\"turn\"}"
        }
    });

    let decoded =
        decode_request_with_headers(body.to_string().as_bytes(), &headers, 64 * 1024 * 1024)
            .expect("request should decode");

    assert_eq!(Value::Object(openai_wire_body(&decoded).clone()), body);
    assert_eq!(
        openai_protocol_context(&decoded),
        &serde_json::Map::from_iter([
            ("turn_state".to_owned(), json!("turn-state")),
            ("turn_metadata".to_owned(), json!("{\"kind\":\"review\"}")),
            ("conversation_id".to_owned(), json!("conversation")),
            (
                "opaque_request_headers".to_owned(),
                json!([
                    ["x-codex-turn-state", STANDARD.encode(b"turn-state")],
                    [
                        "x-codex-turn-metadata",
                        STANDARD.encode(br#"{"kind":"review"}"#)
                    ],
                    ["conversation-id", STANDARD.encode(b"conversation")]
                ]),
            ),
        ])
    );
}

#[test]
fn decoder_should_preserve_ordinary_request_headers_as_opaque_multivalues() {
    let mut headers = HeaderMap::new();
    for name in [
        "chatgpt-organization-id",
        "chatgpt-org-id",
        "x-openai-organization",
        "x-openai-project",
    ] {
        headers.insert(name, HeaderValue::from_static("unclassified-extension"));
    }
    headers.append(
        "x-openai-future-mode",
        HeaderValue::from_static("future-ascii"),
    );
    headers.append(
        "x-openai-future-mode",
        HeaderValue::from_bytes(b"\x80\xff").expect("opaque header bytes"),
    );
    headers.append("version", HeaderValue::from_static("future-v1"));
    headers.append(
        "version",
        HeaderValue::from_bytes(b"future-\x80").expect("opaque known header bytes"),
    );
    headers.insert(
        "accept",
        HeaderValue::from_static("application/vnd.openai.responses+json"),
    );
    headers.insert(
        "content-type",
        HeaderValue::from_static("application/vnd.openai.responses.request+json"),
    );
    headers.insert(
        "user-agent",
        HeaderValue::from_static("Codex future-client"),
    );
    headers.insert("originator", HeaderValue::from_static("future-codex"));
    headers.append(
        "openai-beta",
        HeaderValue::from_static("future_responses=v2"),
    );
    headers.append("openai-beta", HeaderValue::from_static("future_tools=v3"));
    headers.insert(
        "x-openai-internal-codex-residency",
        HeaderValue::from_static("future-region"),
    );
    headers.insert(
        "x-codex-turn-state",
        HeaderValue::from_static("same-account-turn"),
    );

    headers.insert(
        "connection",
        HeaderValue::from_static("keep-alive, x-private-hop"),
    );
    headers.insert("x-private-hop", HeaderValue::from_static("drop-me"));
    headers.insert("host", HeaderValue::from_static("downstream.invalid"));
    headers.insert("content-length", HeaderValue::from_static("999"));
    headers.insert("authorization", HeaderValue::from_static("Bearer client"));
    headers.insert("x-api-key", HeaderValue::from_static("client-key"));
    headers.insert(
        "x-openai-actor-authorization",
        HeaderValue::from_static("proxy-managed"),
    );
    headers.insert("cookie", HeaderValue::from_static("client=cookie"));
    headers.insert(
        "chatgpt-account-id",
        HeaderValue::from_static("client-account"),
    );
    headers.insert(
        "chatgpt-project-id",
        HeaderValue::from_static("client-project"),
    );
    headers.insert(
        "x-codex-installation-id",
        HeaderValue::from_static("client-installation"),
    );
    headers.insert(
        "x-oai-attestation",
        HeaderValue::from_static("client-attestation"),
    );
    headers.insert("x-oai-is", HeaderValue::from_static("client-is"));
    headers.insert(
        "x-oai-is-update",
        HeaderValue::from_static("client-is-update"),
    );

    let decoded = decode_request_with_headers(
        br#"{"model":"smart-code","input":"hello"}"#,
        &headers,
        64 * 1024 * 1024,
    )
    .expect("opaque request headers should not affect body decoding");
    let entries = openai_protocol_context(&decoded)
        .get("opaque_request_headers")
        .and_then(Value::as_array)
        .expect("opaque request header context");
    let values = |target: &str| {
        entries
            .iter()
            .filter_map(Value::as_array)
            .filter(|entry| entry.first().and_then(Value::as_str) == Some(target))
            .filter_map(|entry| entry.get(1).and_then(Value::as_str))
            .map(|encoded| STANDARD.decode(encoded).expect("valid base64 header value"))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        values("x-openai-future-mode"),
        vec![b"future-ascii".to_vec(), b"\x80\xff".to_vec()]
    );
    assert_eq!(
        openai_protocol_context(&decoded).get("version"),
        Some(&json!("future-v1"))
    );
    assert_eq!(
        values("openai-beta"),
        vec![b"future_responses=v2".to_vec(), b"future_tools=v3".to_vec()]
    );
    for name in [
        "chatgpt-organization-id",
        "chatgpt-org-id",
        "x-openai-organization",
        "x-openai-project",
    ] {
        assert_eq!(values(name), vec![b"unclassified-extension".to_vec()]);
    }
    assert_eq!(
        values("x-codex-installation-id"),
        vec![b"client-installation".to_vec()]
    );
    for preserved in [
        "accept",
        "content-type",
        "x-openai-internal-codex-residency",
        "x-codex-turn-state",
    ] {
        assert_eq!(values(preserved).len(), 1, "missing {preserved}");
    }
    for excluded in [
        "connection",
        "x-private-hop",
        "host",
        "content-length",
        "authorization",
        "x-api-key",
        "x-openai-actor-authorization",
        "cookie",
        "chatgpt-account-id",
        "chatgpt-project-id",
        // 上游指纹由运行时画像统一生成，客户端不得覆盖。
        "user-agent",
        "originator",
        "version",
        "x-oai-attestation",
        "x-oai-is",
        "x-oai-is-update",
    ] {
        assert!(values(excluded).is_empty(), "unexpected {excluded}");
    }
}

#[test]
fn decoder_should_strip_http_transport_but_leave_source_headers_for_provider() {
    let mut headers = HeaderMap::new();
    for name in [
        "cf-visitor",
        "cf-connecting-ip",
        "cf-connecting-ipv6",
        "cf-pseudo-ipv4",
        "cf-ray",
        "cf-ipcountry",
        "cf-warp-tag-id",
        "cf-worker",
        "cf-ew-via",
        "cdn-loop",
        "via",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-prefix",
        "accept-encoding",
    ] {
        headers.insert(name, HeaderValue::from_static("downstream-only"));
    }
    headers.insert(
        "CF-Visitor",
        HeaderValue::from_static(r#"{"scheme":"https"}"#),
    );
    headers.insert("accept-encoding", HeaderValue::from_static("br, gzip"));
    // 正文未压缩；入口现在消费 Content-Encoding，但仍不得向上游透传。
    headers.insert("content-encoding", HeaderValue::from_static("identity"));
    headers.insert("x-openai-future-mode", HeaderValue::from_static("keep"));

    let decoded = decode_request_with_headers(
        br#"{"model":"smart-code","input":"hello"}"#,
        &headers,
        64 * 1024 * 1024,
    )
    .expect("decode request behind a reverse proxy");

    assert_eq!(
        openai_protocol_context(&decoded).get("opaque_request_headers"),
        Some(&Value::Array(
            headers
                .iter()
                .filter(|(name, _)| !matches!(
                    name.as_str(),
                    "accept-encoding" | "content-encoding"
                ))
                .map(|(name, value)| json!([name.as_str(), STANDARD.encode(value.as_bytes())]))
                .collect()
        )),
    );
}

#[test]
fn downstream_client_headers_should_remain_opaque_without_losing_session_semantics() {
    for canonical in [None, Some("canonical-session")] {
        let mut headers = HeaderMap::new();
        for name in [
            "X-Stainless-Runtime",
            "x-stainless-future-field",
            "Origin",
            "Referer",
            "Sec-Ch-Ua",
            "sec-ch-ua-platform",
            "Sec-Fetch-Site",
            "session_id",
        ] {
            headers.append(name, HeaderValue::from_static("alias-session"));
            headers.append(name, HeaderValue::from_static("duplicate"));
        }
        if let Some(session) = canonical {
            headers.insert("session-id", HeaderValue::from_static(session));
        }
        for name in [
            "thread-id",
            "x-client-request-id",
            "traceparent",
            "tracestate",
        ] {
            headers.insert(name, HeaderValue::from_static("keep"));
        }
        headers.append("x-future-business", HeaderValue::from_static("first"));
        headers.append(
            "x-future-business",
            HeaderValue::from_bytes(b"\x80\xff").unwrap(),
        );
        let original_headers = headers.clone();
        let body = json!({
            "model": "smart-code", "input": "中文 pi 内容不能被请求头过滤改写",
            "prompt_cache_key": "synthetic-cache",
            "client_metadata": {"session_id": "body-session"}
        });
        let mut frame = body.clone();
        frame["type"] = json!("response.create");
        let opening = OpenAiRequestHeaders::from_headers(&headers);
        for decoded in [
            decode_request_with_headers(body.to_string().as_bytes(), &headers, 64 * 1024 * 1024)
                .unwrap(),
            decode_response_create_with_context(&frame.to_string(), &opening).unwrap(),
            decode_response_create_with_context(&frame.to_string(), &opening).unwrap(),
        ] {
            assert_eq!(openai_wire_body(&decoded), body.as_object().unwrap());
            let context = openai_protocol_context(&decoded);
            assert_eq!(context["session_id"], canonical.unwrap_or("alias-session"));
            let entries = context["opaque_request_headers"].as_array().unwrap();
            for name in [
                "x-stainless-runtime",
                "x-stainless-future-field",
                "origin",
                "referer",
                "sec-ch-ua",
                "sec-ch-ua-platform",
                "sec-fetch-site",
                "session_id",
            ] {
                let values: Vec<_> = entries
                    .iter()
                    .filter(|entry| entry[0] == name)
                    .cloned()
                    .collect();
                assert_eq!(
                    values,
                    vec![
                        json!([name, STANDARD.encode(b"alias-session")]),
                        json!([name, STANDARD.encode(b"duplicate")]),
                    ],
                    "source header {name} belongs to the Provider"
                );
            }
            for name in [
                "thread-id",
                "x-client-request-id",
                "traceparent",
                "tracestate",
            ] {
                assert!(
                    entries
                        .iter()
                        .any(|entry| entry == &json!([name, STANDARD.encode(b"keep")]))
                );
            }
            let future: Vec<_> = entries
                .iter()
                .filter(|entry| entry[0] == "x-future-business")
                .cloned()
                .collect();
            assert_eq!(
                future,
                vec![
                    json!(["x-future-business", STANDARD.encode(b"first")]),
                    json!(["x-future-business", STANDARD.encode(b"\x80\xff")]),
                ]
            );
        }
        // 解码不修改原始请求，CORS、鉴权和本地观测仍可读取原值。
        assert_eq!(headers, original_headers);
    }
}

#[test]
fn decoder_should_preserve_unknown_nested_values_without_debug_disclosure() {
    let secret = "nested-private-value";
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hello", "future": secret}]}],
        "future_top_level": {"secret": secret}
    }));

    assert_eq!(
        Value::Object(openai_wire_body(&decoded).clone()).pointer("/input/0/content/0/future"),
        Some(&json!(secret))
    );
    assert!(!format!("{decoded:?}").contains(secret));
}

#[test]
fn decoder_should_preserve_unknown_arbitrary_precision_numbers() {
    let number = "12345678901234567890123456789012345678901234567890";
    let body = format!(r#"{{"model":"smart-code","input":"hello","future_number":{number}}}"#);

    let decoded = decode_request_with_headers(body.as_bytes(), &HeaderMap::new(), 64 * 1024 * 1024)
        .expect("opaque numeric field should decode");
    let encoded = serde_json::to_string(openai_wire_body(&decoded)).expect("encode request body");

    assert!(encoded.contains(&format!(r#""future_number":{number}"#)));
}

#[test]
fn decoder_should_preserve_opaque_continuation_and_mark_the_routing_requirement() {
    for response_id in [
        "resp_private_continuation".to_owned(),
        format!("resp_{}\0opaque", "x".repeat(257)),
        String::new(),
    ] {
        let decoded = generate_request(json!({
            "model": "smart-code",
            "input": "continue",
            "previous_response_id": response_id.clone()
        }));

        assert!(matches!(
            decoded.metadata().continuation(),
            ContinuationIntent::PreviousResponseId(value) if value == &response_id
        ));
        assert_eq!(
            openai_wire_body(&decoded)
                .get("previous_response_id")
                .and_then(Value::as_str),
            Some(response_id.as_str())
        );
        assert!(
            decoded
                .operation()
                .capability_requirements()
                .features()
                .contains(&Feature::NativeContinuation)
        );
        if !response_id.is_empty() {
            assert!(!format!("{decoded:?}").contains(&response_id));
        }
    }
}

#[test]
fn decoder_should_leave_openai_semantic_validation_to_the_upstream() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "stream": "future-invalid-value",
        "max_output_tokens": 0,
        "future_official_field": [1, 2, 3]
    }));

    assert_eq!(
        Value::Object(openai_wire_body(&decoded).clone()),
        json!({
            "model": "smart-code",
            "stream": "future-invalid-value",
            "max_output_tokens": 0,
            "future_official_field": [1, 2, 3]
        })
    );
    assert_eq!(generate_operation(&decoded).max_output_tokens(), None);
}

#[test]
fn decoder_should_not_reject_large_bodies_using_catalog_context_limits() {
    let body = json!({
        "model": "model-a",
        "input": "x".repeat(128_001)
    })
    .to_string();
    let decoded = decode_request(body.as_bytes()).expect("large request should decode");

    let snapshot = super::snapshot("sk_context_test", "openai");
    let plan = snapshot
        .plan(
            &PublicModelId::new(decoded.metadata().requested_model()).expect("public model"),
            decoded.operation(),
            snapshot.all_account_scope(),
            &RoutingContext::default(),
        )
        .expect("large body must not be locally context-gated");

    assert_eq!(
        plan.candidates()[0]
            .upstream_model()
            .expect("model route candidate")
            .as_str(),
        "model-a"
    );
}

#[test]
fn decoder_should_return_safe_errors_for_invalid_envelopes() {
    let malformed = decode_request(br#"{"model":"smart-code","input":"private-prompt"#)
        .expect_err("truncated JSON must fail");
    let non_object = decode_request(br#"["smart-code"]"#).expect_err("array must fail");
    let missing_model = decode_request(br#"{"input":"hello"}"#).expect_err("model is required");

    assert_eq!(malformed, RequestDecodeError::MalformedJson);
    assert_eq!(non_object, RequestDecodeError::ExpectedObject);
    assert_eq!(
        missing_model,
        RequestDecodeError::MissingField {
            field: "model".to_owned()
        }
    );
    assert!(!format!("{malformed:?} {malformed}").contains("private-prompt"));
}

#[test]
fn transparent_encoder_should_forward_raw_sse_frames_unknown_events_and_terminal_response() {
    let response_id = "resp_upstream";
    let started = ResponseMeta::new(response_id, "gpt-test");
    let completed = ResponseMeta::new(response_id, "gpt-test");
    let raw_partial = Bytes::from_static(
        b"id: evt_partial\r\nevent: response.image_generation_call.partial_image\r\nretry: 2000\r\ndata: {\"type\":\"response.image_generation_call.partial_image\",\"response_id\":\"resp_upstream\",\"opaque\":true}\r\n\r\n",
    );
    let partial_data = json!({
        "type": "response.image_generation_call.partial_image",
        "response_id": response_id,
        "opaque": true
    });
    let terminal_response = json!({
        "id": response_id,
        "status": "completed",
        "output": [{"type": "image_generation_call", "result": "opaque-image"}],
        "future_terminal_field": {"keep": true}
    });
    let events = [
        openai_wire_event(
            vec![GatewayEvent::Started(started)],
            "response.created",
            json!({"type": "response.created", "response": {"id": response_id, "status": "in_progress"}}),
        ),
        ProviderEvent::wire(
            ProtocolWireEvent::json_with_raw_sse_metadata(
                "openai",
                Some("response.image_generation_call.partial_image".to_owned()),
                partial_data.clone(),
                raw_partial.clone(),
                Some("evt_partial".to_owned()),
                Some(2_000),
            )
            .expect("valid raw OpenAI event"),
        ),
        openai_wire_event(
            vec![GatewayEvent::Completed(completed)],
            "response.completed",
            json!({"type": "response.completed", "response": terminal_response}),
        ),
    ];
    let mut encoder = OpenAiResponsesEncoder::new();
    let frames = events
        .iter()
        .flat_map(|event| encoder.push_sse(event))
        .collect::<Vec<_>>();
    let response = encoder.finish().expect("wire response should finish");
    let body = frames
        .iter()
        .flat_map(|frame| frame.as_ref().iter().copied())
        .collect::<Vec<_>>();
    let body_text = std::str::from_utf8(&body).expect("wire SSE should be UTF-8");
    let parsed = parse_sse_events(body_text).expect("wire SSE should parse");

    assert_eq!(frames[1], raw_partial);
    assert_eq!(parsed[1].id.as_deref(), Some("evt_partial"));
    assert_eq!(parsed[1].retry, Some(2_000));
    assert_eq!(
        serde_json::from_str::<Value>(&parsed[1].data).expect("unknown event JSON"),
        partial_data
    );
    assert_eq!(response, terminal_response);
}

#[test]
fn transparent_encoder_should_use_identical_json_for_sse_and_websocket() {
    let response_id = "resp_transport_match";
    let created_data = json!({
        "type": "response.created",
        "response": {"id": response_id, "status": "in_progress", "future": true}
    });
    let completed_data = json!({
        "type": "response.completed",
        "response": {"id": response_id, "status": "completed", "output": []}
    });
    let events = [
        openai_wire_event(
            vec![GatewayEvent::Started(ResponseMeta::new(
                response_id,
                "gpt-test",
            ))],
            "response.created",
            created_data.clone(),
        ),
        openai_wire_event(
            vec![GatewayEvent::Completed(ResponseMeta::new(
                response_id,
                "gpt-test",
            ))],
            "response.completed",
            completed_data.clone(),
        ),
    ];
    let mut sse_encoder = OpenAiResponsesEncoder::new();
    let mut websocket_encoder = OpenAiResponsesEncoder::new();

    let sse = events
        .iter()
        .flat_map(|event| sse_encoder.push_sse(event))
        .collect::<Vec<_>>();
    let websocket = events
        .iter()
        .flat_map(|event| websocket_encoder.push_websocket(event))
        .collect::<Vec<_>>();
    let sse_text = String::from_utf8(
        sse.iter()
            .flat_map(|frame| frame.as_ref().iter().copied())
            .collect(),
    )
    .expect("SSE is UTF-8");
    let sse_data = parse_sse_events(&sse_text)
        .expect("SSE parses")
        .into_iter()
        .map(|event| event.data)
        .collect::<Vec<_>>();

    assert_eq!(websocket, sse_data);
    assert_eq!(
        websocket,
        vec![created_data.to_string(), completed_data.to_string()]
    );
}

#[test]
fn transparent_encoder_should_translate_error_wire_to_response_failed_for_websocket() {
    // codex 的 WS 端点只消费带 status 的包装错误帧；上游缺少 status 的裸
    // `error` 帧会被静默忽略，客户端只能空等到 idle 超时。WS 边界与 SSE
    // 边界一致投影成 `response.failed`，codex 将其映射为可重试错误并立即
    // 重试，而不是把失败原因拖到流 EOF。
    let response_id = "resp_ws_error";
    let started = openai_wire_event(
        Vec::new(),
        "response.created",
        json!({
            "type": "response.created",
            "response": {"id": response_id, "status": "in_progress"}
        }),
    );
    let error = openai_wire_event(
        Vec::new(),
        "error",
        json!({
            "type": "error",
            "error": {
                "type": "service_unavailable_error",
                "code": "server_is_overloaded",
                "message": "Our servers are currently overloaded. Please try again later."
            },
            "sequence_number": 2
        }),
    );

    let mut websocket_encoder = OpenAiResponsesEncoder::new();
    websocket_encoder.push_websocket(&started);
    let messages = websocket_encoder.push_websocket(&error);

    assert_eq!(messages.len(), 1);
    assert!(websocket_encoder.has_wire_failure());
    let payload: Value = serde_json::from_str(&messages[0]).expect("response.failed is valid JSON");
    assert_eq!(payload["type"], "response.failed");
    assert_eq!(payload["response"]["id"], response_id);
    assert_eq!(payload["response"]["status"], "failed");
    assert_eq!(payload["response"]["error"]["code"], "server_error");

    // 与 SSE 边界投影到同一份 data 负载，两条客户端通道行为一致。
    let mut sse_encoder = OpenAiResponsesEncoder::new();
    sse_encoder.push_sse(&started);
    let frames = sse_encoder.push_sse(&error);
    let sse_text = String::from_utf8(
        frames
            .iter()
            .flat_map(|frame| frame.as_ref().iter().copied())
            .collect(),
    )
    .expect("SSE is UTF-8");
    let sse_data = parse_sse_events(&sse_text)
        .expect("SSE parses")
        .into_iter()
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(sse_data, messages);

    // 带非 2xx status 的包装错误帧是 codex 可直接消费的形状，必须原样透传。
    let mut passthrough_encoder = OpenAiResponsesEncoder::new();
    passthrough_encoder.push_websocket(&started);
    let wrapped = openai_wire_event(
        Vec::new(),
        "error",
        json!({
            "type": "error",
            "status": 429,
            "headers": {"x-request-id": "req_raw"},
            "error": {"type": "custom_type", "code": "custom_code", "message": "raw marker"},
            "future": {"keep": true}
        }),
    );
    let wrapped_messages = passthrough_encoder.push_websocket(&wrapped);
    assert_eq!(
        wrapped_messages,
        vec![wrapped.wire_event().expect("wire").data().to_string()]
    );
    assert!(passthrough_encoder.has_wire_failure());
}

#[test]
fn transparent_encoder_should_follow_wire_when_canonical_identity_changes() {
    let mut encoder = OpenAiResponsesEncoder::new();
    let started = openai_wire_event(
        vec![GatewayEvent::Started(ResponseMeta::new(
            "resp_1", "gpt-test",
        ))],
        "response.created",
        json!({"type": "response.created", "response": {"id": "wire_resp_1"}}),
    );
    encoder.push_sse(&started);
    let changed_identity = openai_wire_event(
        vec![GatewayEvent::Completed(ResponseMeta::new(
            "resp_2", "gpt-test",
        ))],
        "response.completed",
        json!({"type": "response.completed", "response": {"id": "wire_resp_2"}}),
    );

    let completed = encoder.push_sse(&changed_identity);

    assert_eq!(completed.len(), 1);
    assert!(encoder.is_completed());
    assert_eq!(encoder.response_id(), Some("wire_resp_2"));
    assert_eq!(
        encoder
            .finish()
            .expect("wire terminal remains authoritative"),
        json!({"id": "wire_resp_2"})
    );
}

#[test]
fn capacity_failure_projection_preserves_wire_metadata_and_original_events() {
    for code in ["server_is_overloaded", "slow_down"] {
        for (event_type, explicit_type) in [
            ("error", true),
            ("error", false),
            ("response.failed", true),
            ("response.failed", false),
        ] {
            for status in [None, Some(400), Some(429), Some(503)] {
                let error = json!({"code": code, "type": "service_unavailable_error", "message": "busy", "future": 42});
                let mut original = if event_type == "error" {
                    json!({"type": event_type, "error": error, "sequence_number": 3})
                } else {
                    json!({"type": event_type, "response": {"id": "resp_capacity", "status": "failed", "error": error}, "sequence_number": 3})
                };
                if let Some(status) = status {
                    original["status_code"] = json!(status);
                }
                let raw = Bytes::from(format!(
                    "id: capacity\r\nretry: 1700\r\nevent: {event_type}\r\ndata: {original}\r\n\r\n"
                ));
                let event = ProviderEvent::wire(
                    ProtocolWireEvent::json_with_raw_sse_metadata(
                        "openai",
                        explicit_type.then(|| event_type.to_owned()),
                        original.clone(),
                        raw.clone(),
                        Some("capacity".to_owned()),
                        Some(1700),
                    )
                    .expect("wire"),
                );
                let mut sse = OpenAiResponsesEncoder::new();
                let mut ws = OpenAiResponsesEncoder::new();
                let created = openai_wire_event(
                    Vec::new(),
                    "response.created",
                    json!({"type": "response.created", "response": {"id": "resp_capacity", "status": "in_progress"}}),
                );
                sse.push_sse(&created);
                ws.push_websocket(&created);
                let frames = sse.push_sse(&event);
                let parsed =
                    parse_sse_events(std::str::from_utf8(&frames[0]).expect("UTF-8")).expect("SSE");
                let sse_data: Value = serde_json::from_str(&parsed[0].data).expect("JSON");
                assert_eq!(parsed[0].id.as_deref(), Some("capacity"));
                assert_eq!(parsed[0].retry, Some(1700));
                assert_eq!(parsed[0].event.as_deref(), Some("response.failed"));
                assert_eq!(sse_data["response"]["error"]["code"], "server_error");
                assert_eq!(sse_data["response"]["error"]["message"], "busy");
                assert_eq!(sse_data["response"]["error"]["future"], 42);
                let messages = ws.push_websocket(&event);
                let ws_data: Value = serde_json::from_str(&messages[0]).expect("JSON");
                if event_type == "error" && status.is_some() {
                    let mut expected = original.clone();
                    expected["error"]["code"] = json!("server_error");
                    expected["status_code"] = json!(503);
                    assert_eq!(ws_data, expected);
                } else {
                    assert_eq!(ws_data, sse_data);
                }
                assert!(sse.has_wire_failure());
                assert!(ws.has_wire_failure());
                let wire = event.wire_event().expect("original wire");
                assert_eq!(wire.data(), &original);
                assert_eq!(wire.raw_sse_frame(), Some(&raw));
            }
        }
    }
}

#[test]
fn capacity_client_projection_does_not_touch_other_codes_or_non_failure_events() {
    for (event_type, code) in [
        ("response.failed", "rate_limit_exceeded"),
        ("response.failed", "insufficient_quota"),
        ("response.failed", "previous_response_not_found"),
        ("response.future", "server_is_overloaded"),
    ] {
        let original =
            json!({"type": event_type, "response": {"error": {"code": code, "message": "busy"}}});
        let raw = Bytes::from(format!("event: {event_type}\r\ndata: {original}\r\n\r\n"));
        let event = ProviderEvent::wire(
            ProtocolWireEvent::json_with_raw_sse_metadata(
                "openai",
                Some(event_type.to_owned()),
                original.clone(),
                raw.clone(),
                None,
                None,
            )
            .expect("wire"),
        );
        assert_eq!(OpenAiResponsesEncoder::new().push_sse(&event), vec![raw]);
        assert_eq!(
            OpenAiResponsesEncoder::new().push_websocket(&event),
            vec![original.to_string()]
        );
    }
}

#[test]
fn transparent_encoder_should_only_require_wire_terminal_for_buffered_conversion() {
    let mut missing_terminal = OpenAiResponsesEncoder::new();
    let started = openai_wire_event(
        vec![GatewayEvent::Started(ResponseMeta::new(
            "resp_1", "gpt-test",
        ))],
        "response.created",
        json!({"type": "response.created", "response": {"id": "resp_1"}}),
    );

    missing_terminal.push_sse(&started);
    assert_eq!(
        missing_terminal
            .finish()
            .expect_err("terminal response is required"),
        ResponseEncodeError::MissingWireTerminal
    );
}
