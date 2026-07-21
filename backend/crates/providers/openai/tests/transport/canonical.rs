use gateway_core::engine::UpstreamSendState;
use gateway_core::error::ProviderErrorKind;
use gateway_core::event::{ContentKind, FinishReason, GatewayEvent, ProviderEvent};

use provider_openai::transport::canonical::{
    CodexCanonicalDecoder, CodexCanonicalError, CodexCanonicalFailure, CodexCanonicalOutcome,
};

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
            "response.output_text.delta"
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
fn decoder_should_reject_a_changed_upstream_response_id() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_first\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_changed\",\"model\":\"gpt-test\"}}\n\n",
    );

    let failure = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("response ID changes must fail closed");

    let CodexCanonicalError::Protocol(error) = failure.error() else {
        panic!("response ID changes are protocol failures");
    };
    assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
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
