use gateway_core::engine::UpstreamSendState;
use gateway_core::error::ProviderErrorKind;
use gateway_core::event::{ContentKind, FinishReason, GatewayEvent};

use provider_openai::CodexCanonicalDecoder;

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

    assert!(events.iter().any(|event| matches!(
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

    assert!(matches!(events[0], GatewayEvent::Started(_)));
    assert!(matches!(
        events[1],
        GatewayEvent::ContentAdded(ref item) if item.kind() == ContentKind::Text
    ));
    assert!(matches!(events[2], GatewayEvent::TextDelta(_)));
    assert!(matches!(events[3], GatewayEvent::Usage(_)));
    assert!(matches!(
        events[4],
        GatewayEvent::Completed(ref meta)
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

    assert!(matches!(
        events.as_slice(),
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
fn decoder_should_fail_closed_for_unknown_events_without_echoing_body() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.secret_future_event\n",
        "data: {\"secret\":\"must-not-leak\"}\n\n",
    );
    let error = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("unknown event must fail closed");

    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(!format!("{error:?}").contains("must-not-leak"));
    assert!(!format!("{error}").contains("must-not-leak"));
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
    assert!(matches!(events.as_slice(), [GatewayEvent::Completed(_)]));
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

    assert!(matches!(events.last(), Some(GatewayEvent::Completed(_))));
}

#[test]
fn decoder_should_classify_official_token_invalid_failure() {
    assert_failed_event(
        "token_invalid",
        ProviderErrorKind::Unauthorized,
        "auth-secret-marker",
    );
}

#[test]
fn decoder_should_classify_official_model_not_supported_failure() {
    assert_failed_event(
        "model_not_supported",
        ProviderErrorKind::Unsupported,
        "model-secret-marker",
    );
}

#[test]
fn decoder_should_classify_official_quota_failure() {
    assert_failed_event(
        "quota_exceeded",
        ProviderErrorKind::QuotaExhausted,
        "quota-secret-marker",
    );
}

#[test]
fn decoder_should_classify_official_server_overloaded_failure() {
    assert_failed_event(
        "server_is_overloaded",
        ProviderErrorKind::Unavailable,
        "server-secret-marker",
    );
}

#[test]
fn decoder_should_classify_official_cyber_policy_as_an_invalid_request() {
    assert_failed_event(
        "cyber_policy",
        ProviderErrorKind::InvalidRequest,
        "policy-secret-marker",
    );
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

    let error = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("response ID changes must fail closed");

    assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
}

fn assert_failed_event(code: &str, expected: ProviderErrorKind, marker: &str) {
    let body = format!(
        "event: response.failed\ndata: {{\"response\":{{\"id\":\"resp_failed\",\"status\":\"failed\",\"error\":{{\"code\":\"{code}\",\"message\":\"{marker}\"}}}}}}\n\n"
    );
    let error = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("failed event must become a typed error");

    assert_eq!(error.kind(), expected);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(!format!("{error:?} {error}").contains(marker));
}

fn assert_incomplete_reason(reason: &str, expected: FinishReason) {
    let body = format!(
        "event: response.created\ndata: {{\"response\":{{\"id\":\"resp_incomplete\",\"model\":\"gpt-test\"}}}}\n\nevent: response.incomplete\ndata: {{\"response\":{{\"id\":\"resp_incomplete\",\"model\":\"gpt-test\",\"status\":\"incomplete\",\"incomplete_details\":{{\"reason\":\"{reason}\"}}}}}}\n\n"
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("incomplete response is terminal");
    let finish_reason = events.iter().find_map(|event| match event {
        GatewayEvent::Completed(meta) => meta.finish_reason(),
        _ => None,
    });

    assert_eq!(finish_reason, Some(expected));
}
