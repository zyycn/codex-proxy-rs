mod http;
mod websocket;

use gateway_api::openai::responses::{
    ContinuationIntent, RequestDecodeError, ResponseEncodeError, ResponsesCollector, decode_request,
};
use gateway_core::{
    accounting::{CalculatedCost, ProviderReportedCost, Usage},
    event::{
        ContentItem, ContentKind, EventSequenceError, FinishReason, GatewayEvent, ReasoningDelta,
        ResponseMeta, TextDelta, ToolCallDelta,
    },
    operation::{
        ContentPart, Feature, MessageRole, Operation, OutputFormat, ReasoningEffort,
        ReasoningSummary, ResponsePersistence,
    },
};
use gateway_protocol::openai::sse::parse_sse_events;
use serde_json::{Value, json};

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
fn decoder_should_map_string_input_to_user_text() {
    let decoded = generate_request(json!({"model": "smart-code", "input": "hello"}));
    let message = &generate_operation(&decoded).messages()[0];

    assert!(matches!(
        (message.role(), &message.content()[0]),
        (MessageRole::User, ContentPart::Text(text)) if text == "hello"
    ));
}

#[test]
fn decoder_should_default_to_buffered_stored_response() {
    let decoded = generate_request(json!({"model": "smart-code", "input": "hello"}));

    assert_eq!(
        (
            decoded.metadata().stream(),
            decoded.metadata().store(),
            generate_operation(&decoded).response_persistence(),
        ),
        (false, true, ResponsePersistence::Store)
    );
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
fn decoder_should_reject_zero_max_output_tokens() {
    let error = decode_request(
        json!({"model": "smart-code", "input": "hello", "max_output_tokens": 0})
            .to_string()
            .as_bytes(),
    )
    .expect_err("zero output limit must fail");

    assert_eq!(
        error,
        RequestDecodeError::InvalidValue {
            field: "max_output_tokens".to_owned()
        }
    );
}

#[test]
fn decoder_should_reject_fractional_max_output_tokens() {
    let error = decode_request(
        json!({"model": "smart-code", "input": "hello", "max_output_tokens": 1.5})
            .to_string()
            .as_bytes(),
    )
    .expect_err("fractional output limit must fail");

    assert_eq!(
        error,
        RequestDecodeError::InvalidType {
            field: "max_output_tokens".to_owned(),
            expected: "a non-negative integer"
        }
    );
}

#[test]
fn decoder_should_reject_an_unscoped_previous_response_id() {
    let error = decode_request(
        json!({
            "model": "smart-code",
            "input": "continue",
            "previous_response_id": "upstream-response-id"
        })
        .to_string()
        .as_bytes(),
    )
    .expect_err("non-gateway response ID must fail");

    assert_eq!(
        error,
        RequestDecodeError::InvalidValue {
            field: "previous_response_id".to_owned()
        }
    );
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
fn decoder_should_reject_inline_image_data() {
    let error = decode_request(
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
    .expect_err("inline image data must fail");

    assert_eq!(
        error,
        RequestDecodeError::InvalidValue {
            field: "input[0].content[0].image_url".to_owned()
        }
    );
}

#[test]
fn decoder_should_report_nested_unknown_field_path_without_its_value() {
    let secret = "nested-private-value";
    let error = decode_request(
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
    .expect_err("nested unknown field must fail");
    let rendered = format!("{error:?}\n{error}\n{}", error.protocol_body().into_value());

    assert_eq!(
        error,
        RequestDecodeError::UnknownField {
            field: "input[0].content[0].secret_value".to_owned()
        }
    );
    assert!(!rendered.contains(secret));
}

#[test]
fn decoder_should_reject_unknown_top_level_field() {
    let error = decode_request(
        json!({"model": "smart-code", "input": "hello", "mystery": true})
            .to_string()
            .as_bytes(),
    )
    .expect_err("unknown field must fail");

    assert_eq!(
        error,
        RequestDecodeError::UnknownField {
            field: "mystery".to_owned()
        }
    );
}

#[test]
fn decoder_should_reject_known_unsupported_semantic() {
    let error = decode_request(
        json!({"model": "smart-code", "input": "hello", "background": true})
            .to_string()
            .as_bytes(),
    )
    .expect_err("known unsupported field must fail");

    assert_eq!(
        error,
        RequestDecodeError::UnsupportedField {
            field: "background".to_owned()
        }
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
        json!({"model": "smart-code", "input": secret, "background": true})
            .to_string()
            .as_bytes(),
    )
    .expect_err("unsupported field must fail");
    let rendered = format!("{error:?}\n{error}\n{}", error.protocol_body().into_value());

    assert!(!rendered.contains(secret) && !rendered.contains("private prompt"));
}

#[test]
fn stream_terminal_response_should_equal_non_stream_response() {
    let events = text_events();
    let collected = ResponsesCollector::collect(1_700_000_000, &events)
        .expect("canonical events should encode");
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
    let mut sse_collector = ResponsesCollector::new(1_700_000_000);
    let mut websocket_collector = ResponsesCollector::new(1_700_000_000);

    for event in &events {
        let sse_frames = sse_collector.push(event).expect("SSE event encoding");
        let websocket_events = websocket_collector
            .push_websocket_events(event)
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
    let collected = ResponsesCollector::collect(1_700_000_000, &events)
        .expect("canonical events should encode");
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
    let collected = ResponsesCollector::collect(1_700_000_000, &events)
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
    let first =
        ResponsesCollector::collect(1_700_000_000, &events).expect("first encoding should work");
    let second =
        ResponsesCollector::collect(1_700_000_000, &events).expect("second encoding should work");

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
    let collected = ResponsesCollector::collect(1_700_000_000, &events)
        .expect("length termination should encode");
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
    let collected = ResponsesCollector::collect(1_700_000_000, &events)
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
