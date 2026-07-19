use gateway_core::event::{ContentKind, FinishReason, GatewayEvent};

use provider_xai::{GrokCanonicalDecoder, grok_billing_breakdown};

fn terminal_cost_events(
    model: &str,
    input_tokens: u64,
    cached_tokens: u64,
    output_tokens: u64,
    provider_cost_ticks: Option<u64>,
) -> Vec<GatewayEvent> {
    let provider_cost = provider_cost_ticks
        .map(|ticks| format!(",\"cost_in_usd_ticks\":{ticks}"))
        .unwrap_or_default();
    let body = format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_cost\",\"model\":{model:?}}}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_cost\",\"model\":{model:?},\"status\":\"completed\",\"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":{output_tokens},\"input_tokens_details\":{{\"cached_tokens\":{cached_tokens},\"cache_write_tokens\":0}}{provider_cost}}}}}}}\n\n",
        ),
        model = model,
        input_tokens = input_tokens,
        output_tokens = output_tokens,
        cached_tokens = cached_tokens,
        provider_cost = provider_cost,
    );
    GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical cost response")
}

fn calculated_cost_ticks(events: &[GatewayEvent]) -> Option<u128> {
    events.iter().find_map(|event| match event {
        GatewayEvent::CalculatedCost(cost) => Some(cost.total().amount().scaled()),
        _ => None,
    })
}

fn provider_cost_ticks(events: &[GatewayEvent]) -> Option<u128> {
    events.iter().find_map(|event| match event {
        GatewayEvent::ProviderCost(cost) => Some(cost.total().amount().scaled()),
        _ => None,
    })
}

#[test]
fn decoder_should_normalize_text_usage_and_completion() {
    let body = concat!(
        "event: response.queued\n",
        "data: {\"type\":\"response.queued\"}\n\n",
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"grok-code-test\"}}\n\n",
        "event: response.content_part.added\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\"}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"grok-code-test\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5,\"cost_in_usd_ticks\":37756000}}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
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
        GatewayEvent::ProviderCost(cost)
            if cost.total().amount().scaled() == 37_756_000
                && cost.total().currency().as_str() == "USD"
    ));
    assert!(matches!(
        events[5],
        GatewayEvent::Completed(ref meta) if meta.finish_reason() == Some(FinishReason::Stop)
    ));
}

#[test]
fn decoder_should_leave_cost_unavailable_when_upstream_omits_it() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_no_cost\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_no_cost\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical response");

    assert!(!events.iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_)
    )));
}

#[test]
fn decoder_should_calculate_grok_45_token_cost() {
    let events = terminal_cost_events("grok-4.5", 100, 25, 10, None);

    assert_eq!(calculated_cost_ticks(&events), Some(2_175_000));
    assert_eq!(provider_cost_ticks(&events), None);
}

#[test]
fn billing_breakdown_should_match_the_calculated_terminal_cost() {
    let breakdown = grok_billing_breakdown("grok-4.5", 100, 10, 25).expect("known Grok pricing");

    assert_eq!(breakdown.input_amount().amount().scaled(), 1_500_000);
    assert_eq!(breakdown.cache_read_amount().amount().scaled(), 75_000);
    assert_eq!(breakdown.output_amount().amount().scaled(), 600_000);
    assert_eq!(breakdown.total_amount().amount().scaled(), 2_175_000);
    assert_eq!(breakdown.service_tier(), Some("default"));
}

#[test]
fn decoder_should_price_official_grok_45_build_free_variant() {
    let events = terminal_cost_events("grok-4.5-build-free", 201, 0, 76, Some(0));

    assert_eq!(calculated_cost_ticks(&events), Some(8_580_000));
}

#[test]
fn zero_provider_cost_should_fall_back_to_calculated_cost() {
    let events = terminal_cost_events("grok-4.5", 1, 0, 1, Some(0));

    assert_eq!(calculated_cost_ticks(&events), Some(80_000));
    assert_eq!(provider_cost_ticks(&events), None);
}

#[test]
fn nonzero_provider_cost_should_take_priority_over_calculated_cost() {
    let events = terminal_cost_events("grok-4.5", 100, 25, 10, Some(123));

    assert_eq!(provider_cost_ticks(&events), Some(123));
    assert_eq!(calculated_cost_ticks(&events), None);
}

#[test]
fn decoder_should_apply_long_context_rates_at_threshold() {
    let events = terminal_cost_events("grok-4.5", 200_000, 50_000, 1_000, None);

    assert_eq!(calculated_cost_ticks(&events), Some(6_420_000_000));
}

#[test]
fn decoder_should_price_current_official_text_models() {
    for (model, expected_ticks) in [
        ("grok-4.5", 83_000),
        ("grok-build-0.1", 32_000),
        ("grok-code-fast-1", 32_000),
        ("grok-4.3", 39_500),
        ("grok-4.20-multi-agent-0309", 39_500),
        ("grok-4.20-0309-reasoning", 39_500),
        ("grok-4.20-0309-non-reasoning", 39_500),
    ] {
        let events = terminal_cost_events(model, 2, 1, 1, None);
        assert_eq!(
            calculated_cost_ticks(&events),
            Some(expected_ticks),
            "unexpected calculated cost for {model}"
        );
    }
}

#[test]
fn decoder_should_leave_unpublished_model_pricing_unavailable() {
    let events = terminal_cost_events("grok-composer-2.5-fast", 2, 1, 1, None);

    assert_eq!(calculated_cost_ticks(&events), None);
    assert_eq!(provider_cost_ticks(&events), None);
}

#[test]
fn decoder_should_leave_incomplete_usage_pricing_unavailable() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_partial\",\"model\":\"grok-4.5\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_partial\",\"model\":\"grok-4.5\",\"status\":\"completed\",\"usage\":{\"total_tokens\":3}}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical partial usage response");

    assert_eq!(calculated_cost_ticks(&events), None);
}

#[test]
fn decoder_should_leave_invalid_cached_usage_pricing_unavailable() {
    let events = terminal_cost_events("grok-4.5", 1, 2, 1, None);

    assert_eq!(calculated_cost_ticks(&events), None);
}

#[test]
fn decoder_should_fail_closed_for_non_integer_provider_cost() {
    for invalid in ["-1", "1.5", "\"37756000\"", "null"] {
        let body = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bad_cost\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_bad_cost\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2,\"cost_in_usd_ticks\":",
        )
        .to_owned()
            + invalid
            + "}}}\n\n";
        let error = GrokCanonicalDecoder::new("fallback")
            .push(body.as_bytes())
            .expect_err("invalid provider cost must fail");
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::Protocol
        );
    }
}

#[test]
fn decoder_should_normalize_function_call_and_tool_finish() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tool\",\"model\":\"grok-code-test\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"lookup\"}}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":1}\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool\",\"model\":\"grok-code-test\",\"status\":\"completed\"}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical function call");

    assert!(events.iter().any(|event| matches!(
        event,
        GatewayEvent::Completed(meta)
            if meta.finish_reason() == Some(FinishReason::ToolCall)
    )));
}

#[test]
fn decoder_should_coalesce_reasoning_item_part_and_summary_index() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_reason\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"reason_1\"}}\n\n",
        "event: response.reasoning_summary_part.added\n",
        "data: {\"type\":\"response.reasoning_summary_part.added\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\"}}\n\n",
        "event: response.reasoning_summary_text.delta\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"summary_index\":0,\"delta\":\"thinking\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_reason\",\"status\":\"completed\"}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical reasoning");

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GatewayEvent::ContentAdded(_)))
            .count(),
        1
    );
    assert!(matches!(events[2], GatewayEvent::ReasoningDelta(_)));
}

#[test]
fn decoder_should_fail_closed_for_unknown_or_mismatched_events() {
    for body in [
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            "event: response.backend_tool_call.started\n",
            "data: {\"type\":\"response.backend_tool_call.started\",\"secret\":\"must-not-leak\"}\n\n",
        ),
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n",
        ),
    ] {
        let error = GrokCanonicalDecoder::new("fallback")
            .push(body.as_bytes())
            .expect_err("unsupported event must fail");
        let debug = format!("{error:?}");

        assert!(!debug.contains("must-not-leak"));
        assert!(error.sensitive_context_was_redacted());
    }
}

#[test]
fn decoder_should_require_terminal_response() {
    let mut decoder = GrokCanonicalDecoder::new("fallback");
    decoder
        .push(
            b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
        )
        .expect("start is valid");

    let error = decoder.finish().expect_err("missing terminal must fail");

    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::Protocol
    );
}

#[test]
fn decoder_should_preserve_incomplete_length_reason() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_short\"}}\n\n",
        "event: response.incomplete\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_short\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("incomplete response");

    assert!(matches!(
        events.last(),
        Some(GatewayEvent::Completed(meta))
            if meta.finish_reason() == Some(FinishReason::Length)
    ));
}

#[test]
fn decoder_should_classify_failed_event_without_retaining_body() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"secret\"}}\n\n",
    );
    let error = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("failed response must surface");

    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::RateLimited
    );
    assert!(!format!("{error:?}").contains("secret"));
}
