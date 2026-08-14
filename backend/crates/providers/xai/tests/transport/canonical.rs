use gateway_core::error::ProviderError;
use gateway_core::event::{ContentKind, FinishReason, GatewayEvent, ProviderEvent};
use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use gateway_protocol::openai::sse::{encode_sse_event, encode_sse_event_with_metadata};
use serde_json::Value;

use provider_xai::{GrokCanonicalDecoder, GrokResponsesRequest, grok_billing_breakdown};

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
    decode_canonical(body.as_bytes()).expect("canonical cost response")
}

fn decode_canonical(body: &[u8]) -> Result<Vec<GatewayEvent>, ProviderError> {
    GrokCanonicalDecoder::new("fallback")
        .push(body)
        .map(|events| {
            events
                .into_iter()
                .flat_map(|event| event.into_parts().0)
                .collect()
        })
}

fn wire_events(events: &[ProviderEvent]) -> Vec<&gateway_core::event::ProtocolWireEvent> {
    events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .collect()
}

fn tool_request(body: Value) -> GrokResponsesRequest {
    let Value::Object(body) = body else {
        panic!("request fixture must be an object");
    };
    let request = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    );
    GrokResponsesRequest::encode(
        &request,
        "grok-4.5",
        &ClientApiKeyId::new("key_xai_canonical_tools").expect("client key"),
    )
    .expect("tool request")
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
    let events = decode_canonical(body.as_bytes()).expect("canonical response");

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
fn decoder_output_start_should_ignore_preamble_frames() {
    let mut decoder = GrokCanonicalDecoder::new("fallback");
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_os\",\"model\":\"grok-test\"}}\n\n",
        "event: response.in_progress\n",
        "data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"resp_os\"}}\n\n",
    );
    let _ = decoder.push(body.as_bytes()).expect("preamble frames");
    assert!(!decoder.take_output_start());
}

#[test]
fn decoder_output_start_should_count_structural_output_item_added() {
    let mut decoder = GrokCanonicalDecoder::new("fallback");
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_os\",\"model\":\"grok-test\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
    );
    let _ = decoder.push(body.as_bytes()).expect("output start frame");
    assert!(decoder.take_output_start());
}

#[test]
fn decoder_should_emit_each_complete_text_event_without_waiting_for_the_stream_end() {
    let mut decoder = GrokCanonicalDecoder::new("grok-4.5");
    let created = decoder
        .push(
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_live\",\"model\":\"grok-4.5\"}}\n\n",
            )
            .as_bytes(),
        )
        .expect("created event");
    let delta = decoder
        .push(
            concat!(
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"live\"}\n\n",
            )
            .as_bytes(),
        )
        .expect("text delta");

    assert!(
        created
            .iter()
            .filter_map(ProviderEvent::wire_event)
            .any(|event| event.event_type() == Some("response.created"))
    );
    assert!(delta.iter().any(|event| {
        event.wire_event().is_some_and(|wire| {
            wire.event_type() == Some("response.output_text.delta")
                && wire.data().get("delta") == Some(&serde_json::json!("live"))
        })
    }));
}

#[test]
fn decoder_should_preserve_image_tool_tokens_in_canonical_usage() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_image_usage\",\"model\":\"grok-4.5\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_image_usage\",\"model\":\"grok-4.5\",\"status\":\"completed\",\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"total_tokens\":17},\"tool_usage\":{\"image_gen\":{\"input_tokens\":31,\"output_tokens\":9}}}}\n\n",
    );
    let events = decode_canonical(body.as_bytes()).expect("canonical image usage response");
    let image_usage = events.iter().find_map(|event| match event {
        GatewayEvent::Usage(usage) => Some((usage.image_input_tokens, usage.image_output_tokens)),
        _ => None,
    });

    assert_eq!(image_usage, Some((Some(31), Some(9))));
}

#[test]
fn decoder_should_leave_cost_unavailable_when_upstream_omits_it() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_no_cost\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_no_cost\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
    );
    let events = decode_canonical(body.as_bytes()).expect("canonical response");

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
    let events = decode_canonical(body.as_bytes()).expect("canonical partial usage response");

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
fn decoder_should_tolerate_content_field_error_without_breaking_stream() {
    // 内容事件的字段校验失败（此处空 delta）不应打断已开始的客户端流。
    // 旧行为在此断整条流；修复后跳过该事件的 canonical 提取、wire 原样转发，并继续解出终态。
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_a3\",\"model\":\"grok-code-test\"}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_a3\",\"model\":\"grok-code-test\",\"status\":\"completed\"}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("content field error must not break the stream");
    // 空 delta 的 wire 帧仍原样转发给客户端（透明）。
    assert!(
        wire_events(&events)
            .iter()
            .any(|wire| wire.data().get("delta") == Some(&serde_json::json!(""))),
        "malformed delta wire frame should still be forwarded to the client"
    );
    // 终态仍被解出。
    let canonical: Vec<GatewayEvent> = events
        .into_iter()
        .flat_map(|event| event.into_parts().0)
        .collect();
    assert!(
        canonical
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_))),
        "stream should still reach completion after a tolerated content error"
    );
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
    let events = decode_canonical(body.as_bytes()).expect("canonical function call");

    assert!(events.iter().any(|event| matches!(
        event,
        GatewayEvent::Completed(meta)
            if meta.finish_reason() == Some(FinishReason::ToolCall)
    )));
}

#[test]
fn decoder_should_restore_namespace_custom_search_and_apply_patch_wire_events() {
    let request = tool_request(serde_json::json!({
        "model": "client",
        "input": "use tools",
        "tools": [
            {"type": "namespace", "name": "workspace", "tools": [{
                "type": "function", "name": "read", "parameters": {"type": "object"}
            }]},
            {"type": "custom", "name": "render"},
            {"type": "tool_search", "execution": "client"},
            {"type": "apply_patch"}
        ]
    }));
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tools\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"item_fn\",\"type\":\"function_call\",\"call_id\":\"call_fn\",\"name\":\"workspace__read\"}}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"item_fn\",\"type\":\"function_call\",\"call_id\":\"call_fn\",\"name\":\"workspace__read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"id\":\"item_custom\",\"type\":\"function_call\",\"call_id\":\"call_custom\",\"name\":\"render\"}}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_custom\",\"call_id\":\"call_custom\",\"output_index\":1,\"delta\":\"{\\\"input\\\":\\\"raw\\\"}\"}\n\n",
        "event: response.function_call_arguments.done\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_custom\",\"call_id\":\"call_custom\",\"output_index\":1,\"arguments\":\"{\\\"input\\\":\\\"raw\\\"}\"}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"id\":\"item_custom\",\"type\":\"function_call\",\"call_id\":\"call_custom\",\"name\":\"render\",\"arguments\":\"{\\\"input\\\":\\\"raw\\\"}\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":2,\"item\":{\"id\":\"item_search\",\"type\":\"function_call\",\"call_id\":\"call_search\",\"name\":\"xai_proxy_tool_search\"}}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":2,\"item\":{\"id\":\"item_search\",\"type\":\"function_call\",\"call_id\":\"call_search\",\"name\":\"xai_proxy_tool_search\",\"arguments\":\"{\\\"goal\\\":\\\"files\\\"}\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":3,\"item\":{\"id\":\"item_patch\",\"type\":\"function_call\",\"call_id\":\"call_patch\",\"name\":\"xai_proxy_apply_patch\"}}\n\n",
        "event: response.function_call_arguments.done\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_patch\",\"call_id\":\"call_patch\",\"output_index\":3,\"arguments\":\"{\\\"operation\\\":{\\\"type\\\":\\\"delete_file\\\",\\\"path\\\":\\\"old.txt\\\"}}\"}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":3,\"item\":{\"id\":\"item_patch\",\"type\":\"function_call\",\"call_id\":\"call_patch\",\"name\":\"xai_proxy_apply_patch\",\"arguments\":\"{\\\"operation\\\":{\\\"type\\\":\\\"delete_file\\\",\\\"path\\\":\\\"old.txt\\\"}}\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tools\",\"status\":\"completed\",\"tools\":[{\"type\":\"function\",\"name\":\"workspace__read\"}]}}\n\n",
    );
    let events = GrokCanonicalDecoder::for_request("grok-4.5", &request)
        .push(body.as_bytes())
        .expect("translated tool stream");
    let wire = wire_events(&events);

    assert!(wire.iter().any(|event| {
        event.data().pointer("/item/name") == Some(&serde_json::json!("read"))
            && event.data().pointer("/item/namespace") == Some(&serde_json::json!("workspace"))
    }));
    assert!(wire.iter().any(|event| {
        event.event_type() == Some("response.custom_tool_call_input.done")
            && event.data().pointer("/input") == Some(&serde_json::json!("raw"))
    }));
    assert!(wire.iter().any(|event| {
        event.data().pointer("/item/type") == Some(&serde_json::json!("tool_search_call"))
            && event.data().pointer("/item/arguments/goal") == Some(&serde_json::json!("files"))
    }));
    assert_eq!(
        wire.iter()
            .filter(|event| event.data().pointer("/item/type")
                == Some(&serde_json::json!("apply_patch_call")))
            .count(),
        2
    );
    assert_eq!(
        wire.last()
            .and_then(|event| event.data().pointer("/response/tools/0/type")),
        Some(&serde_json::json!("namespace"))
    );
}

#[test]
fn decoder_should_restore_custom_apply_patch_arguments_as_raw_input() {
    let patch = concat!(
        "*** Begin Patch\n",
        "*** Update File: src/lib.rs\n",
        "@@\n",
        "-let path = \"old\\\\name\";\n",
        "+let path = \"new\\\\name\";\n",
        "*** End Patch\n",
    );
    let request = tool_request(serde_json::json!({
        "model": "client",
        "input": "edit",
        "tools": [{"type": "custom", "name": "apply_patch"}]
    }));
    let arguments =
        serde_json::to_string(&serde_json::json!({"patch": patch})).expect("apply_patch arguments");
    let arguments_json = serde_json::to_string(&arguments).expect("arguments JSON string");
    let body = format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_custom_patch\"}}}}\n\n",
            "event: response.output_item.added\n",
            "data: {{\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{{\"id\":\"item_custom_patch\",\"type\":\"function_call\",\"call_id\":\"call_custom_patch\",\"name\":\"apply_patch\"}}}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_custom_patch\",\"call_id\":\"call_custom_patch\",\"output_index\":0,\"delta\":{arguments_json}}}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_custom_patch\",\"call_id\":\"call_custom_patch\",\"output_index\":0,\"arguments\":{arguments_json}}}\n\n",
            "event: response.output_item.done\n",
            "data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{{\"id\":\"item_custom_patch\",\"type\":\"function_call\",\"call_id\":\"call_custom_patch\",\"name\":\"apply_patch\",\"arguments\":{arguments_json}}}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_custom_patch\",\"status\":\"completed\"}}}}\n\n",
        ),
        arguments_json = arguments_json,
    );

    let events = GrokCanonicalDecoder::for_request("grok-4.5", &request)
        .push(body.as_bytes())
        .expect("custom apply_patch stream");
    let wire = wire_events(&events);

    assert!(wire.iter().any(|event| {
        event.event_type() == Some("response.custom_tool_call_input.delta")
            && event.data().pointer("/delta") == Some(&serde_json::json!(patch))
    }));
    assert!(wire.iter().any(|event| {
        event.event_type() == Some("response.custom_tool_call_input.done")
            && event.data().pointer("/input") == Some(&serde_json::json!(patch))
    }));
    assert!(wire.iter().any(|event| {
        event.event_type() == Some("response.output_item.done")
            && event.data().pointer("/item/type") == Some(&serde_json::json!("custom_tool_call"))
            && event.data().pointer("/item/input") == Some(&serde_json::json!(patch))
    }));
}

#[test]
fn decoder_should_buffer_only_custom_arguments_until_the_done_event() {
    let request = tool_request(serde_json::json!({
        "model": "client",
        "input": "render",
        "tools": [{"type": "custom", "name": "render"}]
    }));
    let mut decoder = GrokCanonicalDecoder::for_request("grok-4.5", &request);
    decoder
        .push(
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_render\",\"model\":\"grok-4.5\"}}\n\n",
            )
            .as_bytes(),
        )
        .expect("custom response created");
    let added = decoder
        .push(
            concat!(
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"item_render\",\"type\":\"function_call\",\"call_id\":\"call_render\",\"name\":\"render\"}}\n\n",
            )
            .as_bytes(),
        )
        .expect("custom item added");
    let first_delta = decoder
        .push(
            concat!(
                "event: response.function_call_arguments.delta\n",
                "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_render\",\"call_id\":\"call_render\",\"output_index\":0,\"delta\":\"{\\\"input\\\":\\\"hel\"}\n\n",
            )
            .as_bytes(),
        )
        .expect("first custom argument delta");
    let second_delta = decoder
        .push(
            concat!(
                "event: response.function_call_arguments.delta\n",
                "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_render\",\"call_id\":\"call_render\",\"output_index\":0,\"delta\":\"lo\\\"}\"}\n\n",
            )
            .as_bytes(),
        )
        .expect("second custom argument delta");
    let done = decoder
        .push(
            concat!(
                "event: response.function_call_arguments.done\n",
                "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_render\",\"call_id\":\"call_render\",\"output_index\":0,\"arguments\":\"{\\\"input\\\":\\\"hello\\\"}\"}\n\n",
            )
            .as_bytes(),
        )
        .expect("custom argument done");

    assert!(wire_events(&added).iter().any(|event| {
        event.event_type() == Some("response.output_item.added")
            && event.data().pointer("/item/type") == Some(&serde_json::json!("custom_tool_call"))
    }));
    assert!(first_delta.is_empty());
    assert!(second_delta.is_empty());
    assert!(wire_events(&done).iter().any(|event| {
        event.event_type() == Some("response.custom_tool_call_input.done")
            && event.data().get("input") == Some(&serde_json::json!("hello"))
    }));
}

#[test]
fn decoder_should_normalize_schema_integer_arguments_and_resequence_split_events() {
    let request = tool_request(serde_json::json!({
        "model": "client",
        "input": "wait",
        "tools": [{
            "type": "function",
            "name": "wait",
            "parameters": {
                "$ref": "#/$defs/Args",
                "$defs": {
                    "Args": {
                        "type": "object",
                        "properties": {
                            "timeout_ms": {"allOf": [{"type": "integer"}]},
                            "fractional": {"type": "integer"},
                            "unsafe": {"type": "integer"},
                            "list": {
                                "type": "array",
                                "prefixItems": [{"type": "integer"}],
                                "items": {"type": "number"}
                            },
                            "extra": {
                                "type": "object",
                                "additionalProperties": {"type": "integer"}
                            }
                        }
                    },
                    "Unused": {"type": "integer"}
                }
            }
        }]
    }));
    let arguments = concat!(
        "{\"timeout_ms\":60000.0,\"fractional\":1.5,",
        "\"unsafe\":9007199254740992.0,\"list\":[2.0,3.0],",
        "\"extra\":{\"retries\":4.0},\"untyped\":7.0}"
    );
    let body = [
        encode_sse_event(
            "response.created",
            &serde_json::json!({
                "type": "response.created",
                "response": {"id": "resp_integer"}
            })
            .to_string(),
        ),
        encode_sse_event(
            "response.output_item.added",
            &serde_json::json!({
                "type": "response.output_item.added",
                "sequence_number": 40.0,
                "output_index": 0,
                "item": {
                    "id": "item_wait",
                    "type": "function_call",
                    "call_id": "call_wait",
                    "name": "wait"
                }
            })
            .to_string(),
        ),
        encode_sse_event(
            "",
            &serde_json::json!({
                "sequence_number": "invalid",
                "future": true
            })
            .to_string(),
        ),
        encode_sse_event(
            "response.function_call_arguments.delta",
            &serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "sequence_number": 91,
                "item_id": "item_wait",
                "call_id": "call_wait",
                "output_index": 0,
                "delta": arguments
            })
            .to_string(),
        ),
        encode_sse_event_with_metadata(
            "response.function_call_arguments.done",
            &serde_json::json!({
                "type": "response.function_call_arguments.done",
                "sequence_number": 120,
                "item_id": "item_wait",
                "call_id": "call_wait",
                "output_index": 0,
                "arguments": arguments
            })
            .to_string(),
            Some("evt_arguments_done"),
            Some(250),
        ),
        encode_sse_event(
            "response.output_item.done",
            &serde_json::json!({
                "type": "response.output_item.done",
                "sequence_number": 200,
                "output_index": 0,
                "item": {
                    "id": "item_wait",
                    "type": "function_call",
                    "call_id": "call_wait",
                    "name": "wait",
                    "arguments": arguments
                }
            })
            .to_string(),
        ),
    ]
    .concat();

    let events = GrokCanonicalDecoder::for_request("grok-4.5", &request)
        .push(body.as_bytes())
        .expect("integer argument stream");
    let wire = wire_events(&events);
    let sequences = wire
        .iter()
        .filter_map(|event| event.data().get("sequence_number")?.as_u64())
        .collect::<Vec<_>>();

    assert_eq!(sequences, vec![40, 41, 42, 43, 44]);
    assert!(wire.iter().any(|event| {
        event.event_type().is_none()
            && event.data().get("future") == Some(&serde_json::json!(true))
            && event.data().get("sequence_number") == Some(&serde_json::json!(41))
    }));

    let argument_events = wire
        .iter()
        .filter(|event| {
            matches!(
                event.event_type(),
                Some(
                    "response.function_call_arguments.delta"
                        | "response.function_call_arguments.done"
                        | "response.output_item.done"
                )
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(argument_events.len(), 3);
    for event in &argument_events {
        let raw = event
            .data()
            .pointer(if event.event_type() == Some("response.output_item.done") {
                "/item/arguments"
            } else if event.event_type() == Some("response.function_call_arguments.delta") {
                "/delta"
            } else {
                "/arguments"
            })
            .and_then(Value::as_str)
            .expect("normalized arguments");
        let normalized = serde_json::from_str::<Value>(raw).expect("argument JSON");
        assert_eq!(
            normalized.pointer("/timeout_ms"),
            Some(&serde_json::json!(60000))
        );
        assert_eq!(
            normalized.pointer("/fractional"),
            Some(&serde_json::json!(1.5))
        );
        assert_eq!(
            normalized.pointer("/unsafe"),
            Some(&serde_json::json!(9007199254740992.0))
        );
        assert_eq!(normalized.pointer("/list/0"), Some(&serde_json::json!(2)));
        assert_eq!(normalized.pointer("/list/1"), Some(&serde_json::json!(3.0)));
        assert_eq!(
            normalized.pointer("/extra/retries"),
            Some(&serde_json::json!(4))
        );
        assert_eq!(
            normalized.pointer("/untyped"),
            Some(&serde_json::json!(7.0))
        );
    }
    assert_eq!(argument_events[0].sse_id(), Some("evt_arguments_done"));
    assert_eq!(argument_events[0].sse_retry(), Some(250));
    assert_eq!(argument_events[1].sse_id(), None);
    assert_eq!(argument_events[1].sse_retry(), None);
}

#[test]
fn decoder_should_fail_open_after_the_function_argument_buffer_limit() {
    let request = tool_request(serde_json::json!({
        "model": "client",
        "input": "wait",
        "tools": [{
            "type": "function",
            "name": "wait",
            "parameters": {
                "type": "object",
                "properties": {"timeout_ms": {"type": "integer"}}
            }
        }]
    }));
    let arguments = format!(
        "{{\"timeout_ms\":1.0,\"padding\":\"{}\"}}",
        "x".repeat((1 << 20) + 1)
    );
    let mut decoder = GrokCanonicalDecoder::for_request("grok-4.5", &request);
    let added = encode_sse_event(
        "response.output_item.added",
        &serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "id": "item_large",
                "type": "function_call",
                "call_id": "call_large",
                "name": "wait"
            }
        })
        .to_string(),
    );
    decoder
        .push(added.as_bytes())
        .expect("large function item added");

    let delta = encode_sse_event(
        "response.function_call_arguments.delta",
        &serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "item_large",
            "call_id": "call_large",
            "output_index": 0,
            "delta": arguments
        })
        .to_string(),
    );
    let delta = decoder
        .push(delta.as_bytes())
        .expect("oversized arguments should pass through");
    let delta_wire = wire_events(&delta);
    assert_eq!(delta_wire.len(), 1);
    assert_eq!(
        delta_wire[0].data().get("delta").and_then(Value::as_str),
        Some(arguments.as_str())
    );

    let done = encode_sse_event(
        "response.function_call_arguments.done",
        &serde_json::json!({
            "type": "response.function_call_arguments.done",
            "item_id": "item_large",
            "call_id": "call_large",
            "output_index": 0,
            "arguments": arguments
        })
        .to_string(),
    );
    let done = decoder
        .push(done.as_bytes())
        .expect("oversized arguments done");
    let normalized = wire_events(&done)[0]
        .data()
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
        .expect("normalized final arguments");
    assert_eq!(normalized.get("timeout_ms"), Some(&serde_json::json!(1)));
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
    let events = decode_canonical(body.as_bytes()).expect("canonical reasoning");

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
fn decoder_should_preserve_unknown_events_as_openai_wire() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
        "event: xai.internal.trace\n",
        "data: {\"type\":\"xai.internal.trace\",\"secret\":\"must-not-reach-client\"}\n\n",
        "event: response.doom_loop_check \n",
        "data: {\"type\":\"response.backend_tool_call.started\",\"secret\":\"drop-header\"}\n\n",
        "event: response.backend_tool_call.started\n",
        "data: {\"type\":\"response.doom_loop_check\",\"secret\":\"drop-body\"}\n\n",
        "event: response.backend_tool_call.started\n",
        "id: evt_future\n",
        "retry: 1250\n",
        "data: {\"type\":\"response.backend_tool_call.started\",\"future\":{\"nested\":[1,true]}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\"}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("unknown wire event");
    let wire = wire_events(&events);

    assert_eq!(wire.len(), 3);
    assert_eq!(wire[1].protocol(), "openai");
    assert_eq!(
        wire[1].event_type(),
        Some("response.backend_tool_call.started")
    );
    assert_eq!(wire[1].sse_id(), Some("evt_future"));
    assert_eq!(wire[1].sse_retry(), Some(1_250));
    assert_eq!(
        wire[1].data().pointer("/future/nested"),
        Some(&serde_json::json!([1, true]))
    );
    assert!(events[1].canonical_facts().is_empty());
}

#[test]
fn decoder_should_rewrite_grok_ping_frames_as_sse_comments() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_ping\"}}\n\n",
        "event: ping\n",
        "data: {\"type\":\"ping\",\"x-opencode-type\":\"inference-cost\",\"cost\":2.75}\n\n",
        "event: ping\n",
        "data: {not-json}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ping\",\"status\":\"completed\"}}\n\n",
    );

    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("ping-filtered response");
    let wire = wire_events(&events);
    let ping_comments = wire
        .iter()
        .filter(|event| event.raw_sse_frame().map(AsRef::as_ref) == Some(b": ping\n\n".as_slice()))
        .count();

    assert_eq!(ping_comments, 2);
    assert!(wire.iter().all(|event| event.event_type() != Some("ping")));
}

#[test]
fn decoder_should_preserve_image_and_hosted_tool_items_without_inventing_facts() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_items\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"image_generation_call\",\"id\":\"image_1\",\"future\":true}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"web_search_call\",\"id\":\"search_1\",\"status\":\"searching\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_items\",\"status\":\"completed\"}}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("opaque output items");

    assert_eq!(wire_events(&events).len(), 4);
    assert!(events[1].canonical_facts().is_empty());
    assert!(events[2].canonical_facts().is_empty());
    assert_eq!(
        events[1]
            .wire_event()
            .expect("image wire")
            .data()
            .pointer("/item/future"),
        Some(&serde_json::json!(true))
    );
}

#[test]
fn decoder_should_hide_injected_search_calls_and_compact_later_output_indexes() {
    let request = tool_request(serde_json::json!({
        "model": "client",
        "prompt_cache_key": "cache-session",
        "input": "hello"
    }));
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cache\",\"tools\":[{\"type\":\"web_search\"},{\"type\":\"x_search\"}]}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_xs\",\"call_id\":\"xs_call_1\",\"name\":\"x_semantic_search\",\"arguments\":\"{}\"}}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"item_id\":\"item_xs\",\"call_id\":\"xs_call_1\",\"delta\":\"{}\"}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_xs\",\"call_id\":\"xs_call_1\",\"name\":\"x_semantic_search\",\"arguments\":\"{}\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"image_generation_call\",\"id\":\"image_visible\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cache\",\"status\":\"completed\",\"tools\":[{\"type\":\"web_search\"},{\"type\":\"x_search\"}],\"output\":[{\"type\":\"function_call\",\"call_id\":\"xs_call_1\",\"name\":\"x_semantic_search\",\"arguments\":\"{}\"},{\"type\":\"image_generation_call\",\"id\":\"image_visible\"}]}}\n\n",
    );

    let events = GrokCanonicalDecoder::for_request("grok-4.5", &request)
        .push(body.as_bytes())
        .expect("filtered cache route response");
    let wire = wire_events(&events);

    assert_eq!(wire.len(), 3);
    assert_eq!(
        wire[1].data().get("output_index"),
        Some(&serde_json::json!(0))
    );
    assert_eq!(
        wire[2].data().pointer("/response/output/0/id"),
        Some(&serde_json::json!("image_visible"))
    );
    assert_eq!(wire[2].data().pointer("/response/output/1"), None);
    assert_eq!(wire[2].data().pointer("/response/tools"), None);
}

#[test]
fn decoder_should_preserve_an_internal_search_name_declared_by_the_client() {
    let request = tool_request(serde_json::json!({
        "model": "client",
        "prompt_cache_key": "cache-session",
        "input": "hello",
        "tools": [{
            "type": "function",
            "name": "x_user_search",
            "parameters": {"type": "object"}
        }]
    }));
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_declared\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_client\",\"call_id\":\"call_client\",\"name\":\"x_user_search\",\"arguments\":\"{}\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_declared\",\"status\":\"completed\"}}\n\n",
    );

    let events = GrokCanonicalDecoder::for_request("grok-4.5", &request)
        .push(body.as_bytes())
        .expect("client-declared search function");

    assert!(wire_events(&events).iter().any(|event| {
        event.data().pointer("/item/name") == Some(&serde_json::json!("x_user_search"))
    }));
}

#[test]
fn decoder_should_tolerate_mismatched_event_identity_and_preserve_the_wire_event() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\"},\"future_field\":true}\n\n",
    );
    let events = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("event and body type disagreement must not terminate the stream");
    let wire = wire_events(&events);

    assert_eq!(wire.len(), 2);
    assert_eq!(wire[1].event_type(), Some("response.output_text.delta"));
    assert_eq!(
        wire[1].data().pointer("/future_field"),
        Some(&Value::Bool(true))
    );
    assert!(
        events[1]
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
    );
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
    let events = decode_canonical(body.as_bytes()).expect("incomplete response");

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
        "data: {\"type\":\"response.failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"type\":\"server_error\",\"message\":\"secret\"}}\n\n",
    );
    let error = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("failed response must surface");

    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::RateLimited
    );
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("rate_limit_exceeded")
    );
    let visible = error
        .client_visible_upstream_error()
        .expect("structured upstream message is client-visible");
    assert_eq!(visible.message(), "secret");
    assert_eq!(visible.code(), Some("rate_limit_exceeded"));
    assert_eq!(visible.error_type(), Some("server_error"));
    assert!(!format!("{error:?}").contains("secret"));
}

#[test]
fn decoder_should_classify_free_quota_failed_event() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_quota\"}}\n\n",
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"error\":{\"code\":\"subscription:free-usage-exhausted\",\"type\":\"billing_error\",\"message\":\"You have used all your free usage\"}}\n\n",
    );
    let error = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("free quota failure must surface");

    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::QuotaExhausted
    );
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("subscription:free-usage-exhausted")
    );
}

#[test]
fn decoder_should_scrub_account_fingerprints_from_failure_messages() {
    let body = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"Too many requests for team 00000000-0000-0000-0000-000000000013 and model grok-4.\\nRequests per Second (actual/limit): 2/2\"}\n\n",
    );
    let error = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("error event must surface");

    let visible = error
        .client_visible_upstream_error()
        .expect("structured upstream message is client-visible");
    assert_eq!(
        visible.message(),
        "Too many requests for team [redacted] and model grok-4. Requests per Second (actual/limit): 2/2"
    );
    assert!(!format!("{error:?}").contains("00000000-0000"));
}

#[test]
fn decoder_should_omit_client_details_for_unstructured_failures() {
    let body = concat!(
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"error\":{\"code\":\"internal_error\"}}\n\n",
    );
    let error = GrokCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("failed response must surface");

    assert!(error.client_visible_upstream_error().is_none());
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("internal_error")
    );
}
