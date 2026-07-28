mod http;
mod websocket;

use axum::http::{HeaderMap, HeaderValue};
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
    decode_request_with_headers(body, &HeaderMap::new())
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
fn decoder_should_preserve_connection_metadata_outside_the_openai_wire_body() {
    let mut headers = HeaderMap::new();
    headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-state"));
    headers.insert(
        "x-codex-turn-metadata",
        HeaderValue::from_static("{\"kind\":\"review\"}"),
    );
    headers.insert("conversation-id", HeaderValue::from_static("conversation"));
    let body = json!({"model": "smart-code", "input": "hello"});

    let decoded = decode_request_with_headers(body.to_string().as_bytes(), &headers)
        .expect("request should decode");

    assert_eq!(Value::Object(openai_wire_body(&decoded).clone()), body);
    assert_eq!(
        openai_protocol_context(&decoded),
        &serde_json::Map::from_iter([
            ("turn_state".to_owned(), json!("turn-state")),
            ("turn_metadata".to_owned(), json!("{\"kind\":\"review\"}")),
            ("conversation_id".to_owned(), json!("conversation")),
        ])
    );
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

    let plan = super::snapshot("sk_context_test", "openai")
        .plan(
            &PublicModelId::new(decoded.metadata().public_model()).expect("public model"),
            decoded.operation(),
            &RoutingContext::default(),
        )
        .expect("large body must not be locally context-gated");

    assert_eq!(plan.candidates()[0].upstream_model().as_str(), "model-a");
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
        .flat_map(|event| encoder.push_sse(event).expect("wire event should encode"))
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
        .flat_map(|event| sse_encoder.push_sse(event).expect("SSE wire event"))
        .collect::<Vec<_>>();
    let websocket = events
        .iter()
        .flat_map(|event| {
            websocket_encoder
                .push_websocket(event)
                .expect("WebSocket wire event")
        })
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
fn transparent_encoder_should_require_matching_canonical_and_wire_terminals() {
    let mut encoder = OpenAiResponsesEncoder::new();
    let started = openai_wire_event(
        vec![GatewayEvent::Started(ResponseMeta::new(
            "resp_1", "gpt-test",
        ))],
        "response.created",
        json!({"type": "response.created", "response": {"id": "resp_1"}}),
    );
    encoder.push_sse(&started).expect("started event");
    let changed_identity = openai_wire_event(
        vec![GatewayEvent::Completed(ResponseMeta::new(
            "resp_2", "gpt-test",
        ))],
        "response.completed",
        json!({"type": "response.completed", "response": {"id": "resp_2"}}),
    );

    assert_eq!(
        encoder
            .push_sse(&changed_identity)
            .expect_err("identity changes must fail"),
        ResponseEncodeError::WireIdentityChanged
    );

    let mut missing_terminal = OpenAiResponsesEncoder::new();
    missing_terminal.push_sse(&started).expect("started event");
    assert_eq!(
        missing_terminal
            .finish()
            .expect_err("terminal response is required"),
        ResponseEncodeError::MissingWireTerminal
    );
}
