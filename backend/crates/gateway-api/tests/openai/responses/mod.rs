mod http;
mod websocket;

use axum::http::HeaderMap;
use gateway_api::openai::responses::{
    ContinuationIntent, DecodedResponsesRequest, OpenAiRequestHeaders, OpenAiResponsesEncoder,
    RequestDecodeError, ResponseCreateFrameError, ResponseEncodeError, ResponsesCollector,
    decode_request_with_headers, decode_response_create_with_context,
};
use gateway_core::{
    accounting::{CalculatedCost, ProviderReportedCost, Usage},
    error::SafeUpstreamValue,
    event::{
        CompactionOutput, CompactionSummary, ContentItem, ContentKind, EventSequenceError,
        FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ReasoningDelta, ResponseMeta,
        TextDelta, ToolCallDelta,
    },
    operation::{
        ContentPart, Feature, MessageRole, Operation, OutputFormat, ReasoningEffort,
        ReasoningSummary, ResponsePersistence,
    },
    routing::ProviderKind,
};
use gateway_protocol::openai::sse::parse_sse_events;
use serde_json::{Value, json};

fn openai_provider() -> ProviderKind {
    ProviderKind::new("openai").expect("OpenAI provider kind")
}

fn decode_request(body: &[u8]) -> Result<DecodedResponsesRequest, RequestDecodeError> {
    decode_request_with_headers(body, &HeaderMap::new(), &openai_provider())
}

fn decode_response_create(
    payload: &str,
) -> Result<DecodedResponsesRequest, ResponseCreateFrameError> {
    decode_response_create_with_context(
        payload,
        &OpenAiRequestHeaders::default(),
        &openai_provider(),
    )
}

fn generate_request(body: Value) -> gateway_api::openai::responses::DecodedResponsesRequest {
    decode_request(body.to_string().as_bytes()).expect("test request should decode")
}

fn generate_operation(
    decoded: &gateway_api::openai::responses::DecodedResponsesRequest,
) -> &gateway_core::operation::GenerateRequest {
    match decoded.operation() {
        Operation::Generate(request) => request,
        _ => panic!("Responses decoder must produce Generate"),
    }
}

fn openai_wire_body(
    decoded: &gateway_api::openai::responses::DecodedResponsesRequest,
) -> &serde_json::Map<String, Value> {
    generate_operation(decoded)
        .protocol_payload()
        .expect("OpenAI request should retain wire payload")
        .body()
}

fn response_meta() -> ResponseMeta {
    ResponseMeta::new("resp_gateway_contract", "smart-code")
}

fn completed_meta() -> ResponseMeta {
    response_meta().with_finish_reason(FinishReason::Stop)
}

fn usage() -> Usage {
    let mut usage = Usage::new();
    usage.input_tokens = Some(10);
    usage.output_tokens = Some(4);
    usage.cached_tokens = Some(2);
    usage.reasoning_tokens = Some(1);
    usage.total_tokens = Some(14);
    usage
}

struct CollectedResponses {
    response: Value,
    sse_frames: Vec<String>,
}

impl CollectedResponses {
    fn response(&self) -> &Value {
        &self.response
    }

    fn sse_frames(&self) -> &[String] {
        &self.sse_frames
    }
}

fn collect_responses(
    created_at: u64,
    events: &[GatewayEvent],
) -> Result<CollectedResponses, ResponseEncodeError> {
    let mut collector = ResponsesCollector::new(created_at);
    let mut sse_frames = Vec::new();
    for event in events {
        sse_frames.extend(collector.push(event)?);
    }
    Ok(CollectedResponses {
        response: collector.finish()?,
        sse_frames,
    })
}

fn text_events() -> Vec<GatewayEvent> {
    vec![
        GatewayEvent::Started(response_meta()),
        GatewayEvent::ContentAdded(ContentItem::new(7, ContentKind::Text)),
        GatewayEvent::TextDelta(TextDelta {
            content_index: 7,
            text: "hello".to_owned(),
        }),
        GatewayEvent::Usage(usage()),
        GatewayEvent::Completed(completed_meta()),
    ]
}

#[test]
fn decoder_should_preserve_roles_tools_reasoning_schema_and_provider_options() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": [
            {"role": "system", "content": "system"},
            {"role": "developer", "content": [{"type": "input_text", "text": "developer"}]},
            {"role": "user", "content": "question"},
            {"role": "assistant", "content": [{"type": "output_text", "text": "prior"}]},
            {"type": "function_call_output", "call_id": "call_1", "output": "sunny"}
        ],
        "tools": [{
            "type": "function",
            "name": "weather",
            "description": "Weather lookup",
            "parameters": {"type": "object"},
            "strict": true
        }],
        "text": {"format": {
            "type": "json_schema",
            "name": "weather_result",
            "description": "Structured weather",
            "schema": {"type": "object"},
            "strict": true
        }},
        "reasoning": {"effort": "xhigh", "summary": "detailed"},
        "max_output_tokens": 512,
        "stream": true,
        "store": false,
        "provider_options": {
            "version": "v1",
            "providers": {"xai": {"search_mode": "off"}}
        }
    }));
    let request = generate_operation(&decoded);
    let roles = request
        .messages()
        .iter()
        .map(|message| message.role())
        .collect::<Vec<_>>();
    let reasoning = request.reasoning().expect("reasoning should be present");
    let OutputFormat::JsonSchema(format) = request.output_format() else {
        panic!("json schema should be preserved")
    };

    assert_eq!(
        (
            roles,
            request.tools()[0].strict(),
            format.name(),
            format.description(),
            format.strict(),
            reasoning.effort,
            reasoning.summary,
            request.max_output_tokens(),
            request.response_persistence(),
            decoded.metadata().stream(),
            decoded.metadata().store(),
            request.provider_options().get("xai"),
        ),
        (
            vec![
                MessageRole::System,
                MessageRole::Developer,
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::User,
            ],
            true,
            "weather_result",
            Some("Structured weather"),
            true,
            Some(ReasoningEffort::ExtraHigh),
            Some(ReasoningSummary::Detailed),
            Some(512),
            ResponsePersistence::DoNotStore,
            true,
            false,
            Some(
                json!({"search_mode": "off"})
                    .as_object()
                    .expect("fixture is an object")
            ),
        )
    );
}

#[test]
fn decoder_should_preserve_only_explicit_xai_provider_options() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": "hello",
        "provider_options": {
            "version": "v1",
            "providers": {
                "xai": {"schema_version": 1, "turn_index": "7"}
            }
        }
    }));
    let request = generate_operation(&decoded);

    assert_eq!(
        request.provider_options().get("xai"),
        json!({"schema_version": 1, "turn_index": "7"}).as_object()
    );
}

#[test]
fn decoder_should_preserve_a_bounded_prompt_cache_key_without_debug_exposure() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": "cache this prefix",
        "prompt_cache_key": "private-cache-route"
    }));
    let request = generate_operation(&decoded);

    assert_eq!(request.prompt_cache_key(), Some("private-cache-route"));
    assert!(!format!("{decoded:?}").contains("private-cache-route"));
}

#[test]
fn decoder_should_map_string_input_to_user_text() {
    let decoded = generate_request(json!({"model": "smart-code", "input": "hello"}));
    let message = &generate_operation(&decoded).messages()[0];

    assert!(matches!(
        (message.role(), &message.content()[0]),
        (MessageRole::User, ContentPart::Text(text)) if text == "hello"
    ));
}

#[test]
fn decoder_should_preserve_remote_compaction_trigger_for_openai() {
    let decoded = decode_request(
        &serde_json::to_vec(&json!({
            "model": "smart-code",
            "input": [
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ],
            "stream": true
        }))
        .expect("request JSON"),
    )
    .expect("remote compaction v2 request should decode");
    let upstream_input = openai_wire_body(&decoded)
        .get("input")
        .and_then(Value::as_array)
        .expect("OpenAI input should remain an array");

    assert!(
        upstream_input
            .iter()
            .any(|item| { item.get("type").and_then(Value::as_str) == Some("compaction_trigger") })
    );
}

#[test]
fn decoder_should_default_to_streaming_non_stored_response() {
    let decoded = generate_request(json!({"model": "smart-code", "input": "hello"}));

    assert_eq!(
        (
            decoded.metadata().stream(),
            decoded.metadata().store(),
            generate_operation(&decoded).response_persistence(),
        ),
        (true, false, ResponsePersistence::DoNotStore)
    );
}

#[test]
fn decoder_should_project_boolean_transport_override_without_wire_leak() {
    for (use_websocket, expected) in [(true, "websocket"), (false, "http_sse")] {
        let decoded = generate_request(json!({
            "model": "smart-code",
            "input": "hello",
            "use_websocket": use_websocket
        }));
        let request = generate_operation(&decoded);

        assert!(openai_wire_body(&decoded).get("use_websocket").is_none());
        assert_eq!(
            request
                .provider_options()
                .get("openai")
                .and_then(|options| options.get("transport")),
            Some(&json!(expected))
        );
    }
}

#[test]
fn decoder_should_reject_non_boolean_transport_override_without_disclosure() {
    let secret = "private-transport-marker";
    let error = decode_request(
        json!({"model": "smart-code", "input": "hello", "use_websocket": secret})
            .to_string()
            .as_bytes(),
    )
    .expect_err("transport override must be a boolean");

    assert_eq!(
        error,
        RequestDecodeError::InvalidType {
            field: "use_websocket".to_owned(),
            expected: "a boolean"
        }
    );
    assert!(!format!("{error:?}\n{error}").contains(secret));
}

#[test]
fn decoder_should_prefer_explicit_provider_transport_over_body_override() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": "hello",
        "use_websocket": true,
        "provider_options": {
            "version": "v1",
            "providers": {
                "openai": {"schema_version": 1, "transport": "http_sse"}
            }
        }
    }));

    assert_eq!(
        generate_operation(&decoded)
            .provider_options()
            .get("openai")
            .and_then(|options| options.get("transport")),
        Some(&json!("http_sse"))
    );
    assert!(openai_wire_body(&decoded).get("use_websocket").is_none());
}

#[test]
fn response_create_should_share_transport_override_projection() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "use_websocket": false
        })
        .to_string(),
    )
    .expect("WebSocket response.create should use the shared request decoder");

    assert_eq!(
        generate_operation(&decoded)
            .provider_options()
            .get("openai")
            .and_then(|options| options.get("transport")),
        Some(&json!("http_sse"))
    );
    assert!(openai_wire_body(&decoded).get("use_websocket").is_none());
}

#[test]
fn connection_context_should_attach_without_entering_request_debug() {
    let user_agent = "Codex-CLI/private-client-context";
    let decoded = generate_request(json!({"model": "smart-code", "input": "hello"}))
        .with_client_context(
            Some("203.0.113.9".parse().expect("IP")),
            Some(user_agent.to_owned()),
        );

    assert_eq!(
        (
            decoded.metadata().client_ip(),
            decoded.metadata().user_agent(),
        ),
        (Some("203.0.113.9".parse().expect("IP")), Some(user_agent))
    );
    assert!(!format!("{decoded:?}").contains(user_agent));
}

#[test]
fn decoder_should_prepend_instructions_as_a_developer_message() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "instructions": "keep the response concise",
        "input": "hello"
    }));
    let messages = generate_operation(&decoded).messages();

    assert!(matches!(
        (messages[0].role(), &messages[0].content()[0], messages[1].role()),
        (MessageRole::Developer, ContentPart::Text(text), MessageRole::User)
            if text == "keep the response concise"
    ));
}

#[test]
fn decoder_should_preserve_function_call_output_even_when_empty() {
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": [{
            "type": "function_call_output",
            "call_id": "call_1",
            "output": ""
        }]
    }));
    let part = &generate_operation(&decoded).messages()[0].content()[0];

    assert!(matches!(
        part,
        ContentPart::ToolResult { call_id, output } if call_id == "call_1" && output.is_empty()
    ));
}

#[test]
fn decoder_should_require_vision_capability_for_input_image() {
    let decoded = generate_request(json!({
        "model": "smart-vision",
        "input": [{
            "role": "user",
            "content": [
                {"type": "input_text", "text": "describe"},
                {"type": "input_image", "image_url": "https://example.invalid/image.png"}
            ]
        }]
    }));

    assert!(
        decoded
            .operation()
            .capability_requirements()
            .features()
            .contains(&Feature::Vision)
    );
}

#[test]
fn decoder_should_freeze_a_conservative_input_token_estimate() {
    let body = json!({"model": "smart-code", "input": "hello"}).to_string();
    let decoded = decode_request(body.as_bytes()).expect("valid request");

    assert_eq!(
        decoded
            .operation()
            .capability_requirements()
            .minimum_context_tokens(),
        u64::try_from(body.len()).expect("body length fits u64")
    );
}

#[test]
fn decoder_should_preserve_previous_response_intent_without_exposing_it_in_debug() {
    let response_id = "resp_private_continuation";
    let decoded = generate_request(json!({
        "model": "smart-code",
        "input": "continue",
        "previous_response_id": response_id
    }));

    assert!(
        matches!(decoded.metadata().continuation(), ContinuationIntent::PreviousResponseId(value) if value == response_id)
            && !format!("{decoded:?}").contains(response_id)
            && decoded
                .operation()
                .capability_requirements()
                .features()
                .contains(&Feature::NativeContinuation)
    );
}

#[test]
fn decoder_should_reject_malformed_json_without_retaining_the_body() {
    let secret = br#"{"model":"smart-code","input":"secret-prompt"#;
    let error = decode_request(secret).expect_err("truncated JSON must fail");

    assert_eq!(error, RequestDecodeError::MalformedJson);
    assert!(!format!("{error:?}\n{error}").contains("secret-prompt"));
}

#[test]
fn decoder_should_reject_a_non_object_body() {
    let error =
        decode_request(br#"["smart-code", "hello"]"#).expect_err("top-level array must fail");

    assert_eq!(error, RequestDecodeError::ExpectedObject);
}

#[test]
fn decoder_should_require_model() {
    let error = decode_request(json!({"input": "hello"}).to_string().as_bytes())
        .expect_err("missing model must fail");

    assert_eq!(
        error,
        RequestDecodeError::MissingField {
            field: "model".to_owned()
        }
    );
}

#[test]
fn decoder_should_require_input() {
    let error = decode_request(json!({"model": "smart-code"}).to_string().as_bytes())
        .expect_err("missing input must fail");

    assert_eq!(
        error,
        RequestDecodeError::MissingField {
            field: "input".to_owned()
        }
    );
}

#[test]
fn decoder_should_preserve_zero_max_output_tokens_for_upstream_validation() {
    let decoded = decode_request(
        json!({"model": "smart-code", "input": "hello", "max_output_tokens": 0})
            .to_string()
            .as_bytes(),
    )
    .expect("opaque OpenAI field should pass through");

    assert_eq!(
        (
            generate_operation(&decoded).max_output_tokens(),
            openai_wire_body(&decoded).get("max_output_tokens"),
        ),
        (None, Some(&json!(0)))
    );
}

#[test]
fn decoder_should_preserve_fractional_max_output_tokens_for_upstream_validation() {
    let decoded = decode_request(
        json!({"model": "smart-code", "input": "hello", "max_output_tokens": 1.5})
            .to_string()
            .as_bytes(),
    )
    .expect("opaque OpenAI field should pass through");

    assert_eq!(
        openai_wire_body(&decoded).get("max_output_tokens"),
        Some(&json!(1.5))
    );
}

#[test]
fn decoder_should_accept_an_external_previous_response_id() {
    let decoded = decode_request(
        json!({
            "model": "smart-code",
            "input": "continue",
            "previous_response_id": "upstream-response-id"
        })
        .to_string()
        .as_bytes(),
    )
    .expect("external response ID should pass through");

    assert!(matches!(
        decoded.metadata().continuation(),
        ContinuationIntent::PreviousResponseId(value) if value == "upstream-response-id"
    ));
}

#[test]
fn decoder_should_reject_a_gateway_prefixed_response_id_with_control_characters() {
    let error = decode_request(
        json!({
            "model": "smart-code",
            "input": "continue",
            "previous_response_id": "resp_safe_prefix\nforged_suffix"
        })
        .to_string()
        .as_bytes(),
    )
    .expect_err("control characters must fail even with the gateway prefix");

    assert_eq!(
        error,
        RequestDecodeError::InvalidValue {
            field: "previous_response_id".to_owned()
        }
    );
}

#[test]
fn decoder_should_reject_an_oversized_gateway_response_id() {
    let response_id = format!("resp_{}", "a".repeat(252));
    let error = decode_request(
        json!({
            "model": "smart-code",
            "input": "continue",
            "previous_response_id": response_id
        })
        .to_string()
        .as_bytes(),
    )
    .expect_err("oversized response IDs must fail");

    assert_eq!(
        error,
        RequestDecodeError::InvalidValue {
            field: "previous_response_id".to_owned()
        }
    );
}

#[test]
fn decoder_should_preserve_inline_image_data_without_decoding_it() {
    let decoded = decode_request(
        json!({
            "model": "smart-vision",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": "data:image/png;base64,cHJpdmF0ZQ=="
                }]
            }]
        })
        .to_string()
        .as_bytes(),
    )
    .expect("Provider should own OpenAI image validation");

    assert!(
        decoded
            .operation()
            .capability_requirements()
            .features()
            .contains(&Feature::Vision)
    );
    assert_eq!(
        openai_wire_body(&decoded)
            .get("input")
            .and_then(|input| input.pointer("/0/content/0/image_url")),
        Some(&json!("data:image/png;base64,cHJpdmF0ZQ=="))
    );
}

#[test]
fn decoder_should_preserve_nested_unknown_fields_without_debug_exposure() {
    let secret = "nested-private-value";
    let decoded = decode_request(
        json!({
            "model": "smart-code",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "hello",
                    "secret_value": secret
                }]
            }]
        })
        .to_string()
        .as_bytes(),
    )
    .expect("unknown OpenAI fields should pass through");
    let rendered = format!("{decoded:?}");

    assert_eq!(
        openai_wire_body(&decoded)
            .get("input")
            .and_then(|input| input.pointer("/0/content/0/secret_value")),
        Some(&json!(secret))
    );
    assert!(!rendered.contains(secret));
}

#[test]
fn decoder_should_preserve_unknown_top_level_field() {
    let decoded = decode_request(
        json!({"model": "smart-code", "input": "hello", "mystery": true})
            .to_string()
            .as_bytes(),
    )
    .expect("unknown OpenAI fields should pass through");

    assert_eq!(
        openai_wire_body(&decoded).get("mystery"),
        Some(&json!(true))
    );
}

#[test]
fn decoder_should_preserve_new_official_semantic_without_gateway_enumeration() {
    let decoded = decode_request(
        json!({"model": "smart-code", "input": "hello", "background": true})
            .to_string()
            .as_bytes(),
    )
    .expect("Provider should own OpenAI request evolution");

    assert_eq!(
        openai_wire_body(&decoded).get("background"),
        Some(&json!(true))
    );
}

#[test]
fn decoder_should_reject_unknown_provider_options_version() {
    let error = decode_request(
        json!({
            "model": "smart-code",
            "input": "hello",
            "provider_options": {"version": "v2", "providers": {}}
        })
        .to_string()
        .as_bytes(),
    )
    .expect_err("unknown extension version must fail");

    assert_eq!(error, RequestDecodeError::UnsupportedProviderOptionsVersion);
}

#[test]
fn request_errors_should_not_disclose_prompt_or_raw_body() {
    let secret = "private prompt that must not appear";
    let error = decode_request(
        json!({"model": "smart-code", "input": secret, "stream": "private-invalid-value"})
            .to_string()
            .as_bytes(),
    )
    .expect_err("wire routing field type must fail");
    let rendered = format!("{error:?}\n{error}\n{}", error.protocol_body().into_value());

    assert!(!rendered.contains(secret) && !rendered.contains("private prompt"));
}

#[test]
fn stream_terminal_response_should_equal_non_stream_response() {
    let events = text_events();
    let collected =
        collect_responses(1_700_000_000, &events).expect("canonical events should encode");
    let sse = collected.sse_frames().join("");
    let parsed = parse_sse_events(&sse).expect("generated SSE should parse");
    let terminal = parsed
        .iter()
        .find(|event| event.event.as_deref() == Some("response.completed"))
        .expect("terminal event should exist");
    let terminal_json: Value = serde_json::from_str(&terminal.data).expect("event data is JSON");

    assert_eq!(terminal_json.get("response"), Some(collected.response()));
}

#[test]
fn websocket_events_should_reuse_the_exact_sse_event_json() {
    let events = text_events();
    let mut sse_encoder = OpenAiResponsesEncoder::new(1_700_000_000);
    let mut websocket_encoder = OpenAiResponsesEncoder::new(1_700_000_000);

    for event in &events {
        let provider_event = ProviderEvent::canonical(event.clone());
        let sse_frames = sse_encoder
            .push_sse(&provider_event)
            .expect("SSE event encoding");
        let websocket_events = websocket_encoder
            .push_websocket(&provider_event)
            .expect("WebSocket event encoding");
        let parsed = parse_sse_events(&sse_frames.join(""))
            .expect("generated SSE frames remain parseable")
            .into_iter()
            .map(|event| event.data)
            .collect::<Vec<_>>();

        assert_eq!(websocket_events, parsed);
    }
}

#[test]
fn stream_should_emit_first_terminal_and_usage_once() {
    let events = text_events();
    let collected =
        collect_responses(1_700_000_000, &events).expect("canonical events should encode");
    let parsed =
        parse_sse_events(&collected.sse_frames().join("")).expect("generated SSE should parse");
    let first = parsed
        .iter()
        .filter(|event| event.event.as_deref() == Some("response.created"))
        .count();
    let terminal = parsed
        .iter()
        .filter(|event| event.event.as_deref() == Some("response.completed"))
        .count();
    let non_null_usage = parsed
        .iter()
        .filter_map(|event| serde_json::from_str::<Value>(&event.data).ok())
        .filter(|event| {
            event
                .pointer("/response/usage")
                .is_some_and(|usage| !usage.is_null())
        })
        .count();

    assert_eq!((first, terminal, non_null_usage), (1, 1, 1));
}

#[test]
fn collector_should_encode_reasoning_and_whole_function_call() {
    let events = vec![
        GatewayEvent::Started(response_meta()),
        GatewayEvent::ContentAdded(ContentItem::new(0, ContentKind::Reasoning)),
        GatewayEvent::ReasoningDelta(ReasoningDelta {
            content_index: 0,
            text: "checked constraints".to_owned(),
        }),
        GatewayEvent::ContentAdded(ContentItem::new(1, ContentKind::ToolCall)),
        GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: 1,
            call_id: "call_weather".to_owned(),
            name: Some("weather".to_owned()),
            arguments_delta: "{\"city\":\"Shanghai\"}".to_owned(),
        }),
        GatewayEvent::Completed(response_meta().with_finish_reason(FinishReason::ToolCall)),
    ];
    let collected = collect_responses(1_700_000_000, &events)
        .expect("reasoning and function call should encode");

    assert_eq!(
        (
            collected.response().pointer("/output/0/type"),
            collected.response().pointer("/output/0/summary/0/text"),
            collected.response().pointer("/output/1/type"),
            collected.response().pointer("/output/1/name"),
            collected.response().pointer("/output/1/arguments"),
        ),
        (
            Some(&json!("reasoning")),
            Some(&json!("checked constraints")),
            Some(&json!("function_call")),
            Some(&json!("weather")),
            Some(&json!("{\"city\":\"Shanghai\"}")),
        )
    );
}

#[test]
fn collector_should_generate_deterministic_output_ids() {
    let events = text_events();
    let first = collect_responses(1_700_000_000, &events).expect("first encoding should work");
    let second = collect_responses(1_700_000_000, &events).expect("second encoding should work");

    assert_eq!(
        first.response().pointer("/output/0/id"),
        second.response().pointer("/output/0/id")
    );
}

#[test]
fn collector_should_encode_length_termination_as_incomplete() {
    let mut events = text_events();
    let terminal = events.last_mut().expect("terminal event");
    *terminal = GatewayEvent::Completed(response_meta().with_finish_reason(FinishReason::Length));
    let collected =
        collect_responses(1_700_000_000, &events).expect("length termination should encode");
    let parsed =
        parse_sse_events(&collected.sse_frames().join("")).expect("generated SSE should parse");

    assert_eq!(
        (
            collected.response().pointer("/status"),
            collected.response().pointer("/incomplete_details/reason"),
            parsed.last().and_then(|event| event.event.as_deref()),
        ),
        (
            Some(&json!("incomplete")),
            Some(&json!("max_output_tokens")),
            Some("response.incomplete"),
        )
    );
}

#[test]
fn collector_should_reject_duplicate_usage() {
    let mut collector = ResponsesCollector::new(1_700_000_000);
    collector
        .push(&GatewayEvent::Started(response_meta()))
        .expect("started should encode");
    collector
        .push(&GatewayEvent::Usage(usage()))
        .expect("first usage should be accepted");
    let error = collector
        .push(&GatewayEvent::Usage(usage()))
        .expect_err("duplicate usage must fail");

    assert_eq!(error, ResponseEncodeError::DuplicateUsage);
}

#[test]
fn collector_should_keep_accounting_costs_out_of_the_responses_wire() {
    let cost = ProviderReportedCost::from_usd_ticks(42).expect("valid provider cost");
    let events = vec![
        GatewayEvent::Started(response_meta()),
        GatewayEvent::CalculatedCost(
            CalculatedCost::from_usd_ticks(24).expect("valid calculated cost"),
        ),
        GatewayEvent::ProviderCost(cost),
        GatewayEvent::Completed(completed_meta()),
    ];
    let collected = collect_responses(1_700_000_000, &events)
        .expect("provider cost is accounting-only metadata");

    assert!(collected.response().get("cost").is_none());
    assert_eq!(
        parse_sse_events(&collected.sse_frames().join(""))
            .expect("generated SSE should parse")
            .len(),
        2
    );
}

#[test]
fn collector_should_reject_incomplete_usage() {
    let mut collector = ResponsesCollector::new(1_700_000_000);
    collector
        .push(&GatewayEvent::Started(response_meta()))
        .expect("started should encode");
    let mut incomplete = Usage::new();
    incomplete.input_tokens = Some(1);
    incomplete.output_tokens = Some(1);
    let error = collector
        .push(&GatewayEvent::Usage(incomplete))
        .expect_err("usage without total must fail");

    assert_eq!(error, ResponseEncodeError::IncompleteUsage);
}

#[test]
fn collector_should_reject_terminal_metadata_changes() {
    let mut collector = ResponsesCollector::new(1_700_000_000);
    collector
        .push(&GatewayEvent::Started(response_meta()))
        .expect("started should encode");
    let error = collector
        .push(&GatewayEvent::Completed(
            ResponseMeta::new("resp_gateway_contract", "different-model")
                .with_finish_reason(FinishReason::Stop),
        ))
        .expect_err("frozen response metadata must not change");

    assert_eq!(error, ResponseEncodeError::MetadataChanged);
}

#[test]
fn collector_should_reject_tool_identity_changes() {
    let mut collector = ResponsesCollector::new(1_700_000_000);
    collector
        .push(&GatewayEvent::Started(response_meta()))
        .expect("started should encode");
    collector
        .push(&GatewayEvent::ContentAdded(ContentItem::new(
            1,
            ContentKind::ToolCall,
        )))
        .expect("tool item should encode");
    collector
        .push(&GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: 1,
            call_id: "call_first".to_owned(),
            name: Some("weather".to_owned()),
            arguments_delta: "{".to_owned(),
        }))
        .expect("first tool delta should encode");
    let error = collector
        .push(&GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: 1,
            call_id: "call_rebound".to_owned(),
            name: Some("weather".to_owned()),
            arguments_delta: "}".to_owned(),
        }))
        .expect_err("tool call identity must remain frozen");

    assert_eq!(error, ResponseEncodeError::ToolIdentityChanged { index: 1 });
}

#[test]
fn collector_should_reject_a_stream_without_terminal_event() {
    let mut collector = ResponsesCollector::new(1_700_000_000);
    collector
        .push(&GatewayEvent::Started(response_meta()))
        .expect("started should encode");
    let error = collector.finish().expect_err("unfinished stream must fail");

    assert_eq!(
        error,
        ResponseEncodeError::Sequence(EventSequenceError::MissingCompleted)
    );
}

#[test]
fn collector_should_reject_event_before_started_via_core_validator() {
    let mut collector = ResponsesCollector::new(1_700_000_000);
    let error = collector
        .push(&GatewayEvent::TextDelta(TextDelta {
            content_index: 0,
            text: "out of order".to_owned(),
        }))
        .expect_err("out-of-order event must fail");

    assert_eq!(
        error,
        ResponseEncodeError::Sequence(EventSequenceError::MissingStarted)
    );
}

#[test]
fn collector_should_reject_unrepresentable_media_output() {
    let mut collector = ResponsesCollector::new(1_700_000_000);
    collector
        .push(&GatewayEvent::Started(response_meta()))
        .expect("started should encode");
    let error = collector
        .push(&GatewayEvent::ContentAdded(ContentItem::new(
            0,
            ContentKind::Image,
        )))
        .expect_err("media output must fail explicitly");

    assert_eq!(
        error,
        ResponseEncodeError::UnsupportedContentKind { kind: "image" }
    );
}

#[test]
fn openai_wire_encoder_should_preserve_unknown_media_events_and_hide_upstream_response_id() {
    let upstream_id = "resp_upstream_private";
    let gateway_id = "resp_gateway_public";
    let started = ResponseMeta::new(gateway_id, "gpt-test").with_upstream_response_id(
        SafeUpstreamValue::new(upstream_id).expect("safe upstream response ID"),
    );
    let completed = ResponseMeta::new(gateway_id, "gpt-test")
        .with_finish_reason(FinishReason::Stop)
        .with_upstream_response_id(
            SafeUpstreamValue::new(upstream_id).expect("safe upstream response ID"),
        );
    let events = [
        openai_wire_event(
            vec![GatewayEvent::Started(started)],
            "response.created",
            json!({
                "type": "response.created",
                "response": {"id": upstream_id, "status": "in_progress"}
            }),
        ),
        ProviderEvent::wire(
            ProtocolWireEvent::json_with_sse_metadata(
                "openai",
                Some("response.image_generation_call.partial_image".to_owned()),
                json!({
                    "type": "response.image_generation_call.partial_image",
                    "response_id": upstream_id,
                    "partial_image_b64": "opaque-image-fragment"
                }),
                Some("evt_partial_image".to_owned()),
                Some(2_000),
            )
            .expect("valid OpenAI wire event"),
        ),
        openai_wire_event(
            vec![GatewayEvent::Completed(completed)],
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {
                    "id": upstream_id,
                    "status": "completed",
                    "output": [{"type": "image_generation_call", "result": "opaque-image"}]
                }
            }),
        ),
    ];
    let mut encoder = OpenAiResponsesEncoder::new(1_700_000_000);
    let frames = events
        .iter()
        .flat_map(|event| encoder.push_sse(event).expect("wire event should encode"))
        .collect::<Vec<_>>();
    let response = encoder.finish().expect("wire response should finish");
    let parsed = parse_sse_events(&frames.join("")).expect("wire SSE should parse");

    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[1].id.as_deref(), Some("evt_partial_image"));
    assert_eq!(parsed[1].retry, Some(2_000));
    assert_eq!(
        serde_json::from_str::<Value>(&parsed[1].data)
            .expect("unknown event JSON")
            .get("partial_image_b64"),
        Some(&json!("opaque-image-fragment"))
    );
    assert_eq!(response.get("id"), Some(&json!(gateway_id)));
    assert!(!frames.join("").contains(upstream_id));
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
fn typed_compaction_output_should_encode_the_codex_remote_compaction_v2_contract() {
    let events = [
        GatewayEvent::Started(response_meta()),
        GatewayEvent::CompactionOutput(CompactionOutput::new(
            CompactionSummary::new("plain Grok conversation summary").expect("valid summary"),
        )),
        GatewayEvent::Completed(completed_meta()),
    ];
    let collected = collect_responses(1_700_000_000, &events)
        .expect("typed compaction output should be representable as OpenAI Responses");
    let parsed =
        parse_sse_events(&collected.sse_frames().join("")).expect("compaction SSE should be valid");
    let done_items = parsed
        .iter()
        .filter_map(|event| serde_json::from_str::<Value>(&event.data).ok())
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("response.output_item.done")
        })
        .filter_map(|event| event.get("item").cloned())
        .collect::<Vec<_>>();

    assert_eq!(
        (
            done_items,
            collected.response().pointer("/output").cloned(),
            parsed
                .last()
                .and_then(|event| serde_json::from_str::<Value>(&event.data).ok())
                .and_then(|event| event.get("type").cloned()),
        ),
        (
            vec![json!({
                "type": "compaction",
                "encrypted_content": "plain Grok conversation summary",
            })],
            Some(json!([{
                "type": "compaction",
                "encrypted_content": "plain Grok conversation summary",
            }])),
            Some(json!("response.completed")),
        )
    );
}
