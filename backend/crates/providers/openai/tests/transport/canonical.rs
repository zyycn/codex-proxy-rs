use gateway_core::event::{ContentKind, FinishReason, GatewayEvent, ProviderEvent};

use provider_openai::transport::canonical::{
    CodexCanonicalDecoder, CodexCanonicalError, CodexCanonicalFailure, CodexCanonicalOutcome,
};
use provider_openai::transport::protocol::websocket::websocket_event_to_sse_frame;

const METADATA_PREFIX_FIXTURE: &str = include_str!(
    "../../../../gateway-api/tests/openai/responses/fixtures/http_sse/metadata_only_prefix.sse"
);

#[test]
fn decoder_should_preserve_existing_metadata_fixture_as_openai_wire() {
    let events = CodexCanonicalDecoder::new("fallback")
        .push(METADATA_PREFIX_FIXTURE.as_bytes())
        .expect("metadata fixture should remain open-world");
    let wire_types = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .filter_map(|wire| wire.event_type())
        .collect::<Vec<_>>();

    assert_eq!(
        wire_types,
        vec!["response.created", "codex.rate_limits", "response.metadata"]
    );
    assert!(matches!(
        canonical_facts(&events).as_slice(),
        [GatewayEvent::Started(_)]
    ));
}

#[test]
fn raw_sse_passthrough_should_keep_original_bytes_alongside_canonical_facts() {
    let heartbeat = ": keep-alive\r\n\r\n";
    let created = concat!(
        "id: evt_created\r\n",
        "event: response.created\r\n",
        "retry: 250\r\n",
        "data: { \"type\": \"response.created\", \"response\": { \"id\": \"resp_raw\", \"model\": \"gpt-test\" } }\r\n\r\n",
    );
    let body = format!("{heartbeat}{created}");

    let events = CodexCanonicalDecoder::new("fallback")
        .with_raw_sse_passthrough()
        .push(body.as_bytes())
        .expect("raw SSE should remain deliverable");
    let heartbeat_wire = events[0].wire_event().expect("heartbeat wire");
    let created_wire = events[1].wire_event().expect("created wire");

    assert!(!heartbeat_wire.has_json_data());
    assert_eq!(
        heartbeat_wire.raw_sse_frame().map(AsRef::as_ref),
        Some(heartbeat.as_bytes())
    );
    assert!(created_wire.has_json_data());
    assert_eq!(
        created_wire.raw_sse_frame().map(AsRef::as_ref),
        Some(created.as_bytes())
    );
    assert!(matches!(
        events[1].canonical_facts(),
        [GatewayEvent::Started(metadata)] if metadata.response_id() == "resp_raw"
    ));
}

#[test]
fn websocket_raw_passthrough_should_preserve_upstream_number_bytes() {
    // OpenAI 线路透明代理：WebSocket 上游帧经 reducer→SSE→decoder(raw 透传) 后，
    // data 段必须与上游原文逐字节一致，不得经 serde 往返改写数值/精度。
    // `1e3` 若被重序列化会变成 `1000.0`——用它作为字节改写的探针。
    let upstream = r#"{"type":"response.created","response":{"id":"resp_ws_raw","model":"gpt-test","x_precision":1e3}}"#;
    let frame = websocket_event_to_sse_frame(upstream)
        .expect("client-visible WS event yields an SSE frame");

    let events = CodexCanonicalDecoder::new("fallback")
        .with_raw_sse_passthrough()
        .push(frame.as_bytes())
        .expect("WS-derived SSE frame should remain deliverable");
    let wire = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .find(|wire| wire.has_json_data())
        .expect("response.created wire");
    let raw = wire
        .raw_sse_frame()
        .map(AsRef::as_ref)
        .expect("raw frame bytes");
    let raw = std::str::from_utf8(raw).expect("utf8 raw frame");

    assert!(
        raw.contains("1e3"),
        "raw frame must keep upstream `1e3` verbatim: {raw}"
    );
    assert!(
        !raw.contains("1000.0"),
        "raw frame must not re-serialize the number via serde: {raw}"
    );
    assert!(matches!(
        wire_for_response_created(&events),
        [GatewayEvent::Started(metadata)] if metadata.response_id() == "resp_ws_raw"
    ));
}

fn wire_for_response_created(events: &[ProviderEvent]) -> &[GatewayEvent] {
    events
        .iter()
        .find(|event| {
            event.wire_event().and_then(|wire| wire.event_type()) == Some("response.created")
        })
        .map_or(&[], ProviderEvent::canonical_facts)
}

#[test]
fn raw_sse_passthrough_should_forward_unparseable_frames_without_failure() {
    let raw = b"retry: later\ndata: opaque\n\n";

    let events = CodexCanonicalDecoder::new("fallback")
        .with_raw_sse_passthrough()
        .push(raw)
        .expect("unparseable frame must not abort transparent transport");
    let wire = events[0].wire_event().expect("raw wire event");

    assert!(!wire.has_json_data());
    assert_eq!(
        wire.raw_sse_frame().map(AsRef::as_ref),
        Some(raw.as_slice())
    );
}

#[test]
fn raw_sse_passthrough_should_forward_frames_with_unsafe_event_metadata() {
    // 上游事件名超出 wire 安全上限（256 字节）时剥离该元数据继续下发，
    // 原始帧字节仍原样透传，而不是整帧静默丢弃。
    let event_name = "x".repeat(300);
    let frame = format!("event: {event_name}\ndata: {{\"type\":\"noise\",\"marker\":1}}\n\n");

    let events = CodexCanonicalDecoder::new("fallback")
        .with_raw_sse_passthrough()
        .push(frame.as_bytes())
        .expect("unsafe event metadata must not abort transparent transport");
    let wire = events[0].wire_event().expect("wire event");

    assert_eq!(
        wire.raw_sse_frame().map(AsRef::as_ref),
        Some(frame.as_bytes())
    );
    assert!(wire.has_json_data());
    assert_eq!(wire.event_type(), None);
}

#[test]
fn decoder_should_strip_unsafe_event_metadata_instead_of_dropping_the_event() {
    // 无 raw 帧的路径（WebSocket JSON 投影）同样不允许因元数据违规丢事件：
    // 剥离事件名后 JSON 正文仍完整下发。
    let event_name = "y".repeat(300);
    let frame = format!("event: {event_name}\ndata: {{\"type\":\"noise\",\"marker\":1}}\n\n");

    let events = CodexCanonicalDecoder::new("fallback")
        .push(frame.as_bytes())
        .expect("unsafe event metadata must not drop the event");
    let wire = events[0].wire_event().expect("wire event");

    assert_eq!(wire.event_type(), None);
    assert!(wire.has_json_data());
    assert_eq!(wire.data()["marker"], serde_json::json!(1));
}

#[test]
fn decoder_should_emit_calculated_cost_for_complete_known_model_usage() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cost\",\"model\":\"gpt-5.4\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cost\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":25,\"cache_write_tokens\":0},\"total_tokens\":110}}}\n\n",
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical priced response");

    assert!(canonical_facts(&events).into_iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(cost)
            if cost.total().amount().scaled() == 3_437_500
    )));
}

#[test]
fn decoder_should_normalize_text_usage_and_completion() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.content_part.added\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\"}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-test\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}}\n\n",
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical response");
    let canonical = canonical_facts(&events);

    assert!(matches!(canonical[0], GatewayEvent::Started(_)));
    assert!(matches!(
        canonical[1],
        GatewayEvent::ContentAdded(item) if item.kind() == ContentKind::Text
    ));
    assert!(matches!(canonical[2], GatewayEvent::TextDelta(_)));
    assert!(matches!(canonical[3], GatewayEvent::Usage(_)));
    assert!(matches!(
        canonical[4],
        GatewayEvent::Completed(meta)
            if meta.finish_reason() == Some(FinishReason::Stop)
    ));
}

#[test]
fn decoder_should_restore_done_only_reasoning_and_text_as_canonical_facts() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_done_only\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"plan\"}]}}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"answer\"}]}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_done_only\",\"model\":\"gpt-test\",\"status\":\"completed\"}}\n\n",
    );

    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("done-only output should be canonicalized");
    let facts = canonical_facts(&events);
    let wire_types = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .filter_map(|wire| wire.event_type())
        .collect::<Vec<_>>();

    assert!(facts.iter().any(|event| matches!(
        event,
        GatewayEvent::ReasoningDelta(delta) if delta.text == "plan"
    )));
    assert!(facts.iter().any(|event| matches!(
        event,
        GatewayEvent::TextDelta(delta) if delta.text == "answer"
    )));
    assert_eq!(
        wire_types,
        vec![
            "response.created",
            "response.output_item.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
}

#[test]
fn decoder_timing_signals_should_count_tool_arguments_as_first_token() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tool_timing\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_tool_timing\"}}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\\\"a\\\"}\"}\n\n",
    );
    let mut decoder = CodexCanonicalDecoder::new("fallback");

    let _ = decoder.push(body.as_bytes()).expect("tool argument frame");
    let signals = decoder.take_timing_signals();

    assert!(signals.semantic_output);
    assert!(!signals.reasoning_output);
    assert!(!signals.text_output);
}

#[test]
fn decoder_timing_signals_should_distinguish_reasoning_and_text_output() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_output_timing\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.reasoning_summary_text.delta\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"plan\"}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":1,\"content_index\":0,\"delta\":\"answer\"}\n\n",
    );
    let mut decoder = CodexCanonicalDecoder::new("fallback");

    let _ = decoder.push(body.as_bytes()).expect("output frames");
    let signals = decoder.take_timing_signals();

    assert!(signals.semantic_output);
    assert!(signals.reasoning_output);
    assert!(signals.text_output);
}

#[test]
fn decoder_should_preserve_image_tool_tokens_in_canonical_usage() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_image_usage\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_image_usage\",\"model\":\"gpt-test\",\"status\":\"completed\",\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"total_tokens\":17},\"tool_usage\":{\"image_gen\":{\"input_tokens\":31,\"output_tokens\":9}}}}\n\n",
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical image usage response");
    let image_usage = canonical_facts(&events)
        .into_iter()
        .find_map(|event| match event {
            GatewayEvent::Usage(usage) => {
                Some((usage.image_input_tokens, usage.image_output_tokens))
            }
            _ => None,
        });

    assert_eq!(image_usage, Some((Some(31), Some(9))));
}

#[test]
fn decoder_should_accept_official_codex_metadata_lifecycle_events() {
    let body = concat!(
        "event: codex.rate_limits\n",
        "data: {\"type\":\"codex.rate_limits\",\"rate_limits\":{\"primary\":{\"used_percent\":42.0}}}\n\n",
        "event: response.metadata\n",
        "data: {\"type\":\"response.metadata\",\"x-codex-turn-state\":\"state\"}\n\n",
        "event: response.in_progress\n",
        "data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"resp_metadata\",\"status\":\"in_progress\"}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_metadata\",\"model\":\"gpt-test\",\"status\":\"completed\"}}\n\n",
    );

    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("official Codex metadata lifecycle events");
    let canonical = canonical_facts(&events);

    assert!(matches!(
        canonical.as_slice(),
        [
            GatewayEvent::Started(_),
            GatewayEvent::ContentAdded(_),
            GatewayEvent::TextDelta(_),
            GatewayEvent::Completed(_)
        ]
    ));
}

#[test]
fn decoder_should_accept_whole_function_call_without_argument_deltas() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_tool\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"lookup\"}}\n\n",
        "event: response.output_item.done\n",
        "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":1}\"}}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_tool\",\"model\":\"gpt-test\",\"status\":\"completed\"}}\n\n",
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical function call");
    let tool_deltas = events
        .iter()
        .flat_map(ProviderEvent::canonical_facts)
        .filter_map(|event| match event {
            GatewayEvent::ToolCallDelta(delta) => Some(delta),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(tool_deltas.len(), 2);
    assert_eq!(tool_deltas[0].name.as_deref(), Some("lookup"));
    assert_eq!(tool_deltas[1].arguments_delta, r#"{"q":1}"#);
}

#[test]
fn decoder_should_preserve_unknown_events_without_exposing_wire_data_in_debug() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.secret_future_event\n",
        "id: evt_future\n",
        "retry: 900\n",
        "data: {\"secret\":\"must-not-leak\"}\n\n",
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("unknown OpenAI event should remain wire-only");
    let unknown = events[1]
        .wire_event()
        .expect("unknown event should retain wire data");

    assert_eq!(unknown.event_type(), Some("response.secret_future_event"));
    assert_eq!(unknown.sse_id(), Some("evt_future"));
    assert_eq!(unknown.sse_retry(), Some(900));
    assert_eq!(
        unknown.data().get("secret"),
        Some(&serde_json::json!("must-not-leak"))
    );
    assert!(!format!("{unknown:?}").contains("must-not-leak"));
}

#[test]
fn decoder_should_keep_media_and_hosted_tool_events_as_openai_wire() {
    for event_type in [
        "response.image_generation_call.partial_image",
        "response.audio.delta",
        "response.web_search_call.searching",
        "response.code_interpreter_call.in_progress",
        "response.computer_tool_call.in_progress",
    ] {
        let body = format!(
            "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_open_world\",\"model\":\"gpt-test\"}}}}\n\nevent: {event_type}\ndata: {{\"type\":\"{event_type}\",\"opaque\":{{\"future\":true}}}}\n\n"
        );
        let events = CodexCanonicalDecoder::new("fallback")
            .push(body.as_bytes())
            .expect("open-world event should remain protocol wire");
        let event = &events[1];

        assert!(event.canonical_facts().is_empty(), "{event_type}");
        assert_eq!(
            event.wire_event().and_then(|wire| wire.event_type()),
            Some(event_type)
        );
        assert_eq!(
            event
                .wire_event()
                .and_then(|wire| wire.data().pointer("/opaque/future")),
            Some(&serde_json::json!(true))
        );
    }
}

#[test]
fn decoder_finish_should_parse_a_final_frame_without_blank_line() {
    let mut decoder = CodexCanonicalDecoder::new("fallback");
    let prefix = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_finish\",\"model\":\"gpt-test\"}}\n\n",
    );
    let tail = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_finish\",\"model\":\"gpt-test\",\"status\":\"completed\"}}",
    );
    decoder.push(prefix.as_bytes()).expect("started event");
    decoder.push(tail.as_bytes()).expect("buffer partial frame");

    let events = decoder.finish().expect("finish partial frame");
    assert!(matches!(
        canonical_facts(&events).as_slice(),
        [GatewayEvent::Completed(_)]
    ));
}

#[test]
fn decoder_should_accept_done_only_after_terminal_event() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_done\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_done\",\"model\":\"gpt-test\",\"status\":\"completed\"}}\n\n",
        "data: [DONE]\n\n",
    );

    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("terminal done marker");

    assert!(matches!(
        canonical_facts(&events).last(),
        Some(GatewayEvent::Completed(_))
    ));
}

#[test]
fn decoder_should_classify_official_token_invalid_failure() {
    assert_failed_event("token_invalid", "auth-secret-marker");
}

#[test]
fn decoder_should_classify_official_model_not_supported_failure() {
    assert_failed_event("model_not_supported", "model-secret-marker");
}

#[test]
fn decoder_should_classify_official_quota_failure() {
    assert_failed_event("quota_exceeded", "quota-secret-marker");
}

#[test]
fn decoder_should_classify_official_server_overloaded_failure() {
    assert_failed_event("server_is_overloaded", "server-secret-marker");
}

#[test]
fn decoder_should_classify_official_cyber_policy_as_an_invalid_request() {
    assert_failed_event("cyber_policy", "policy-secret-marker");
}

#[test]
fn raw_sse_failure_should_keep_the_original_frame_before_reporting_the_typed_failure() {
    let raw = concat!(
        "event: response.failed\r\n",
        "data: { \"type\": \"response.failed\", \"response\": { \"id\": \"resp_raw_failed\", \"status\": \"failed\", \"error\": { \"code\": \"rate_limit_exceeded\", \"message\": \"raw failure marker\" } } }\r\n\r\n",
    );

    let failure = CodexCanonicalDecoder::new("fallback")
        .with_raw_sse_passthrough()
        .push(raw.as_bytes())
        .expect_err("response.failed remains a typed lifecycle failure");
    let wire = failure.events()[0]
        .wire_event()
        .expect("upstream failure wire remains deliverable");

    assert_eq!(wire.event_type(), Some("response.failed"));
    assert_eq!(
        wire.raw_sse_frame().map(|frame| frame.as_ref()),
        Some(raw.as_bytes())
    );
    assert!(!format!("{failure:?}").contains("raw failure marker"));
}

#[test]
fn bare_response_failed_should_project_started_identity_with_the_fallback_model() {
    let raw = concat!(
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_bare_failed\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"bare failure\"}}}\n\n",
    );

    let failure = CodexCanonicalDecoder::new("fallback-model")
        .push(raw.as_bytes())
        .expect_err("bare response.failed remains a typed lifecycle failure");

    assert!(matches!(
        canonical_facts(failure.events()).as_slice(),
        [GatewayEvent::Started(metadata)]
            if metadata.response_id() == "resp_bare_failed"
                && metadata.model() == "fallback-model"
    ));
}

#[test]
fn response_failed_without_identity_should_keep_the_upstream_typed_failure() {
    let raw = concat!(
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"missing identity\"}}}\n\n",
    );

    let failure = CodexCanonicalDecoder::new("fallback")
        .push(raw.as_bytes())
        .expect_err("missing identity must not replace the upstream lifecycle failure");

    assert!(matches!(failure.error(), CodexCanonicalError::Upstream(_)));
    assert!(canonical_facts(failure.events()).is_empty());
}

#[test]
fn decoder_should_preserve_same_chunk_output_before_typed_failure() {
    let marker = "same-chunk-secret-marker";
    let body = format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_partial\",\"model\":\"gpt-test\"}}}}\n\n",
            "event: response.content_part.added\n",
            "data: {{\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{{\"type\":\"output_text\"}}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {{\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}}\n\n",
            "event: response.failed\n",
            "data: {{\"type\":\"response.failed\",\"response\":{{\"id\":\"resp_partial\",\"status\":\"failed\",\"error\":{{\"code\":\"rate_limit_exceeded\",\"message\":\"{}\"}}}}}}\n\n"
        ),
        marker
    );

    let failure = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("response.failed must retain the preceding batch");
    let facts = canonical_facts(failure.events());
    let wire_types = failure
        .events()
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .filter_map(|wire| wire.event_type())
        .collect::<Vec<_>>();

    assert!(failure.semantic_output_seen());
    assert!(matches!(facts[0], GatewayEvent::Started(_)));
    assert!(matches!(facts[1], GatewayEvent::ContentAdded(_)));
    assert!(matches!(facts[2], GatewayEvent::TextDelta(_)));
    assert_eq!(
        wire_types,
        vec![
            "response.created",
            "response.content_part.added",
            "response.output_text.delta",
            "response.failed"
        ]
    );
    assert!(!format!("{failure:?}").contains(marker));
}

#[test]
fn decoder_should_map_max_output_tokens_incomplete_to_length() {
    assert_incomplete_reason("max_output_tokens", FinishReason::Length);
}

#[test]
fn decoder_should_map_content_filter_incomplete_to_content_filter() {
    assert_incomplete_reason("content_filter", FinishReason::ContentFilter);
}

#[test]
fn decoder_should_keep_unknown_incomplete_reason_explicit() {
    assert_incomplete_reason("future_reason", FinishReason::Other);
}

#[test]
fn decoder_should_preserve_changed_upstream_response_ids_as_wire() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_first\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_changed\",\"model\":\"gpt-test\"}}\n\n",
    );

    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("response ID changes must not block wire delivery");

    let wire = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .collect::<Vec<_>>();
    assert_eq!(
        wire.iter()
            .filter_map(|event| event.event_type())
            .collect::<Vec<_>>(),
        vec!["response.created", "response.completed"]
    );
    assert_eq!(
        wire.iter()
            .map(|event| event
                .data()
                .pointer("/response/id")
                .and_then(|id| id.as_str()))
            .collect::<Vec<_>>(),
        vec![Some("resp_first"), Some("resp_changed")]
    );
}

fn assert_failed_event(code: &str, marker: &str) {
    let body = format!(
        "event: response.failed\ndata: {{\"response\":{{\"id\":\"resp_failed\",\"status\":\"failed\",\"error\":{{\"code\":\"{code}\",\"message\":\"{marker}\"}}}}}}\n\n"
    );
    let failure = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("failed event must become a typed error");

    let CodexCanonicalError::Upstream(upstream) = failure.error() else {
        panic!("response.failed must preserve its typed upstream failure");
    };
    assert_eq!(upstream.upstream_code.as_deref(), Some(code));
    assert!(!failure.semantic_output_seen());
    assert!(!format!("{failure:?}").contains(marker));
}

fn assert_incomplete_reason(reason: &str, expected: FinishReason) {
    let body = format!(
        "event: response.created\ndata: {{\"response\":{{\"id\":\"resp_incomplete\",\"model\":\"gpt-test\"}}}}\n\nevent: response.incomplete\ndata: {{\"response\":{{\"id\":\"resp_incomplete\",\"model\":\"gpt-test\",\"status\":\"incomplete\",\"incomplete_details\":{{\"reason\":\"{reason}\"}}}}}}\n\n"
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("incomplete response is terminal");
    let finish_reason = canonical_facts(&events)
        .into_iter()
        .find_map(|event| match event {
            GatewayEvent::Completed(meta) => meta.finish_reason(),
            _ => None,
        });

    assert_eq!(finish_reason, Some(expected));
}

trait CanonicalOutcomeAssertions {
    fn expect(self, message: &str) -> Vec<ProviderEvent>;
    fn expect_err(self, message: &str) -> CodexCanonicalFailure;
}

impl CanonicalOutcomeAssertions for CodexCanonicalOutcome {
    fn expect(self, message: &str) -> Vec<ProviderEvent> {
        match self {
            Self::Events(events) => events,
            Self::Failed(failure) => panic!("{message}: {failure:?}"),
        }
    }

    fn expect_err(self, message: &str) -> CodexCanonicalFailure {
        match self {
            Self::Events(events) => panic!("{message}: decoded {} events", events.len()),
            Self::Failed(failure) => failure,
        }
    }
}

fn canonical_facts(events: &[ProviderEvent]) -> Vec<&GatewayEvent> {
    events
        .iter()
        .flat_map(ProviderEvent::canonical_facts)
        .collect()
}
