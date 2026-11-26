//! 验证 Grok 事件转换的模型、用量、首个输出与完成语义

use gateway_core::error::ProviderError;
use gateway_core::event::{ContentKind, FinishReason, GatewayEvent, ProviderEvent};
use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use serde_json::Value;

use provider_xai::{GrokCanonicalDecoder, GrokResponsesRequest};

#[test]
fn decoder_should_bill_the_sent_model_independently_of_the_response_model() {
    for (sent, returned, reported_cost, expected_cost) in [
        ("grok-4.5", Some("grok-future"), None, Some(2_175_000)),
        ("grok-4.5", Some("grok-4.3"), None, Some(2_175_000)),
        ("grok-4.5", None, None, Some(2_175_000)),
        ("grok-future", Some("grok-4.5"), None, None),
        ("grok-latest", Some("grok-4.3"), Some(0), None),
        ("grok-4.5", Some("grok-future"), Some(123), None),
        ("grok-4.5", Some("grok-future"), Some(0), Some(2_175_000)),
    ] {
        let created = serde_json::json!({"type":"response.created","response":{"id":"resp_model_cost","model":returned}});
        let mut completed = serde_json::json!({
            "type":"response.completed",
            "response":{
                "id":"resp_model_cost","model":returned,"status":"completed","output":[],
                "usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":25},"total_tokens":110}
            }
        });
        if let Some(ticks) = reported_cost {
            completed["response"]["usage"]["cost_in_usd_ticks"] = serde_json::json!(ticks);
        }
        let body = format!("data: {created}\n\ndata: {completed}\n\n");
        let mut decoder = GrokCanonicalDecoder::new(sent);
        let events = decoder.push(body.as_bytes()).expect("model cost response");
        let facts: Vec<_> = events
            .into_iter()
            .flat_map(|event| event.into_parts().0)
            .collect();
        assert_eq!(
            calculated_cost_ticks(&facts),
            expected_cost,
            "{sent} -> {returned:?}"
        );
        assert_eq!(
            provider_cost_ticks(&facts),
            reported_cost.filter(|ticks| *ticks > 0)
        );
        assert!(facts.iter().any(|event| matches!(
            event,
            GatewayEvent::Completed(meta) if meta.model() == Some(returned.unwrap_or(sent))
        )));
    }
}

fn terminal_cost_events(
    model: &str,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    provider_cost_ticks: Option<u64>,
) -> Vec<GatewayEvent> {
    terminal_cost_events_with_pricing(
        model,
        input_tokens,
        cached_input_tokens,
        output_tokens,
        provider_cost_ticks,
        None,
    )
}

fn terminal_cost_events_with_pricing(
    model: &str,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    provider_cost_ticks: Option<u64>,
    pricing: Option<gateway_core::metering::ModelPriceOverride>,
) -> Vec<GatewayEvent> {
    let provider_cost = provider_cost_ticks
        .map(|ticks| format!(",\"cost_in_usd_ticks\":{ticks}"))
        .unwrap_or_default();
    let body = format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_cost\",\"model\":{model:?}}}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_cost\",\"model\":{model:?},\"status\":\"completed\",\"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":{output_tokens},\"input_tokens_details\":{{\"cached_tokens\":{cached_input_tokens},\"cache_write_tokens\":0}}{provider_cost}}}}}}}\n\n",
        ),
        model = model,
        input_tokens = input_tokens,
        output_tokens = output_tokens,
        cached_input_tokens = cached_input_tokens,
        provider_cost = provider_cost,
    );
    GrokCanonicalDecoder::new(model)
        .with_pricing(pricing)
        .push(body.as_bytes())
        .expect("canonical cost response")
        .into_iter()
        .flat_map(|event| event.into_parts().0)
        .collect()
}

fn decode_canonical(body: &[u8], upstream_model: &str) -> Result<Vec<GatewayEvent>, ProviderError> {
    GrokCanonicalDecoder::new(upstream_model)
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
    let events = decode_canonical(body.as_bytes(), "fallback").expect("canonical response");

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
    let events =
        decode_canonical(body.as_bytes(), "fallback").expect("canonical image usage response");
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
    let events = decode_canonical(body.as_bytes(), "fallback").expect("canonical response");

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
fn decoder_should_price_official_grok_45_build_free_variant() {
    let events = terminal_cost_events("grok-4.5-build-free", 201, 0, 76, None);

    assert_eq!(calculated_cost_ticks(&events), Some(8_580_000));
}

#[test]
fn zero_provider_cost_should_use_available_token_pricing() {
    for (model, calculated) in [("grok-4.5", Some(80_000)), ("grok-future", None)] {
        let events = terminal_cost_events(model, 1, 0, 1, Some(0));

        assert_eq!(
            (calculated_cost_ticks(&events), provider_cost_ticks(&events)),
            (calculated, None),
            "{model}"
        );
    }
}

fn pricing_events(response: Value, request: Option<&GrokResponsesRequest>) -> Vec<GatewayEvent> {
    pricing_events_with_override(response, request, None)
}

fn pricing_events_with_override(
    mut response: Value,
    request: Option<&GrokResponsesRequest>,
    pricing: Option<gateway_core::metering::ModelPriceOverride>,
) -> Vec<GatewayEvent> {
    response["id"] = serde_json::json!("resp_pricing");
    response["model"] = serde_json::json!("grok-4.6");
    response["status"] = serde_json::json!("completed");
    let created = serde_json::json!({"type": "response.created", "response": {"id": "resp_pricing", "model": "grok-4.6", "service_tier": "priority"}});
    let completed = serde_json::json!({"type": "response.completed", "response": response});
    let body = format!(
        "event: response.created\ndata: {created}\n\nevent: response.completed\ndata: {completed}\n\n"
    );
    let mut decoder = request
        .map_or_else(
            || GrokCanonicalDecoder::new("grok-4.6"),
            |request| GrokCanonicalDecoder::for_request("grok-4.6", request),
        )
        .with_pricing(pricing);
    let events = decoder
        .push(body.as_bytes())
        .expect("计费未知时仍应交付响应");
    let facts: Vec<_> = events
        .into_iter()
        .flat_map(|event| event.into_parts().0)
        .collect();
    assert!(
        facts
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
    );
    facts
}

#[test]
fn custom_pricing_scales_calculations_but_never_changes_reported_cost() {
    let pricing: gateway_core::metering::ModelPriceOverride =
        serde_json::from_value(serde_json::json!({
            "multiplierBps":15000,"bands":{"standard":{
                "input":"3", "output":"12", "cacheRead":"0", "cacheWrite":"0"
            }}
        }))
        .unwrap();
    let mut response = serde_json::json!({"service_tier":"default","output":[],"usage":{
        "input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":20}
    }});
    assert_eq!(
        calculated_cost_ticks(&pricing_events_with_override(
            response.clone(),
            None,
            Some(pricing.clone())
        )),
        Some(5_400_000)
    );
    response["usage"]["input_tokens_details"]["cache_write_tokens"] = serde_json::json!(10);
    assert_eq!(
        calculated_cost_ticks(&pricing_events_with_override(
            response.clone(),
            None,
            Some(pricing.clone())
        )),
        Some(4_950_000)
    );
    response["service_tier"] = serde_json::json!("priority");
    assert_eq!(
        calculated_cost_ticks(&pricing_events_with_override(
            response.clone(),
            None,
            Some(pricing.clone())
        )),
        Some(6_900_000)
    );
    response["usage"]["cost_in_usd_ticks"] = serde_json::json!(123);
    let events = pricing_events_with_override(response, None, Some(pricing));
    assert_eq!(provider_cost_ticks(&events), Some(123));
    assert_eq!(calculated_cost_ticks(&events), None);
}

#[test]
fn billing_should_use_the_final_response_tier_and_preserve_provider_cost() {
    for (tier, ticks, calculated, provider) in [
        ("priority", None, Some(400_000_000), None),
        ("default", None, Some(200_000_000), None),
        ("future", None, None, None),
        ("priority", Some(0), Some(400_000_000), None),
        ("priority", Some(123), None, Some(123)),
    ] {
        let mut response = serde_json::json!({"service_tier": tier, "output": [], "usage": {"input_tokens": 10_000, "output_tokens": 0}});
        if let Some(ticks) = ticks {
            response["usage"]["cost_in_usd_ticks"] = serde_json::json!(ticks);
        }
        let events = pricing_events(response, None);
        assert_eq!(
            (calculated_cost_ticks(&events), provider_cost_ticks(&events)),
            (calculated, provider),
            "{tier}"
        );
    }
}

#[test]
fn billing_should_require_provider_totals_for_server_tools() {
    for kind in [
        "web_search_call",
        "x_search_call",
        "code_interpreter_call",
        "file_search_call",
        "image_generation_call",
        "future_tool_call",
    ] {
        for ticks in [None, Some(0)] {
            let mut response = serde_json::json!({"output": [{"type": kind, "id": "tool_1", "status": "completed"}], "usage": {"input_tokens": 100, "output_tokens": 1}});
            if let Some(ticks) = ticks {
                response["usage"]["cost_in_usd_ticks"] = serde_json::json!(ticks);
            }
            let events = pricing_events(response, None);
            assert_eq!(
                (calculated_cost_ticks(&events), provider_cost_ticks(&events)),
                (None, None),
                "{kind}: {ticks:?}"
            );
        }
    }
    let request =
        tool_request(serde_json::json!({"input": "查资料", "tools": [{"type": "web_search"}]}));
    // 工具结果可能被上游省略，允许调用收费工具时不能仅靠空 output 认定没有附加费
    let mut response =
        serde_json::json!({"output": [], "usage": {"input_tokens": 100, "output_tokens": 1}});
    assert_eq!(
        calculated_cost_ticks(&pricing_events(response.clone(), Some(&request))),
        None
    );
    response["usage"]["cost_in_usd_ticks"] = serde_json::json!(0);
    let events = pricing_events(response, Some(&request));
    assert_eq!(
        (calculated_cost_ticks(&events), provider_cost_ticks(&events)),
        (None, None)
    );
    let response = serde_json::json!({"output": [], "usage": {"input_tokens": 100, "output_tokens": 1, "num_server_side_tool_calls": 1}});
    assert_eq!(calculated_cost_ticks(&pricing_events(response, None)), None);
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
fn decoder_should_not_price_dynamic_or_compatibility_aliases_as_stable_models() {
    for model in [
        "grok",
        "grok-latest",
        "grok-build-latest",
        "grok-4.6-latest",
        "grok-4.5-latest",
        "grok-4.3-latest",
        "grok-code-fast",
        "grok-4.20-multi-agent-latest",
        "grok-4.20-beta",
        "grok-4.20-beta-reasoning",
        "grok-4.20-beta-non-reasoning",
        "grok-4.20-reasoning-gv2",
        "grok-4.20-non-reasoning-gv2",
        "xai/grok-4.5",
        "grok-4.7",
    ] {
        for reported in [None, Some(0), Some(123)] {
            let events = terminal_cost_events(model, 100, 25, 10, reported);
            assert_eq!(
                calculated_cost_ticks(&events),
                None,
                "{model}, reported: {reported:?}"
            );
            assert_eq!(
                provider_cost_ticks(&events),
                reported.filter(|ticks| *ticks > 0).map(u128::from)
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, GatewayEvent::Completed(_)))
            );
        }
    }
}

#[test]
fn explicit_alias_pricing_still_applies_when_provider_cost_is_unreported() {
    let pricing: gateway_core::metering::ModelPriceOverride =
        serde_json::from_value(serde_json::json!({
            "multiplierBps":15000,"bands":{"standard":{
                "input":"3", "output":"12", "cacheRead":"0", "cacheWrite":"0"
            }}
        }))
        .expect("manual price");
    for reported in [None, Some(0), Some(123)] {
        let events = terminal_cost_events_with_pricing(
            "grok-latest",
            100,
            20,
            10,
            reported,
            Some(pricing.clone()),
        );
        assert_eq!(
            calculated_cost_ticks(&events),
            if reported == Some(123) {
                None
            } else {
                Some(5_400_000)
            }
        );
        assert_eq!(
            provider_cost_ticks(&events),
            reported.filter(|ticks| *ticks > 0).map(u128::from)
        );
    }
}

#[test]
fn decoder_should_leave_incomplete_usage_pricing_unavailable() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_partial\",\"model\":\"grok-4.5\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_partial\",\"model\":\"grok-4.5\",\"status\":\"completed\",\"usage\":{\"total_tokens\":3}}}\n\n",
    );
    let events =
        decode_canonical(body.as_bytes(), "grok-4.5").expect("canonical partial usage response");

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
    // 内容事件的字段校验失败（此处空 delta）不应打断已开始的客户端流
    // 跳过该事件的 canonical 提取、wire 原样转发，并继续解出终态
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
    // 空 delta 的 wire 帧仍原样转发给客户端（透明）
    assert!(
        wire_events(&events)
            .iter()
            .any(|wire| wire.data().get("delta") == Some(&serde_json::json!(""))),
        "malformed delta wire frame should still be forwarded to the client"
    );
    // 终态仍被解出
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
    let events = decode_canonical(body.as_bytes(), "fallback").expect("canonical function call");

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
    let events = decode_canonical(body.as_bytes(), "fallback").expect("canonical reasoning");

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
        "data: {\"type\":\"xai.internal.trace\",\"secret\":\"native-wire-field\"}\n\n",
        "event: response.doom_loop_check \n",
        "data: {\"type\":\"response.backend_tool_call.started\",\"secret\":\"native-header\"}\n\n",
        "event: response.backend_tool_call.started\n",
        "data: {\"type\":\"response.doom_loop_check\",\"secret\":\"native-body\"}\n\n",
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

    assert_eq!(wire.len(), 6);
    let bytes: Vec<u8> = wire
        .iter()
        .flat_map(|event| event.raw_sse_frame().unwrap().to_vec())
        .collect();
    assert_eq!(bytes, body.as_bytes());
    assert_eq!(wire[4].protocol(), "openai");
    assert_eq!(
        wire[4].event_type(),
        Some("response.backend_tool_call.started")
    );
    assert_eq!(wire[4].sse_id(), Some("evt_future"));
    assert_eq!(wire[4].sse_retry(), Some(1_250));
    assert_eq!(
        wire[4].data().pointer("/future/nested"),
        Some(&serde_json::json!([1, true]))
    );
    assert!(events[1].canonical_facts().is_empty());
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
fn decoder_should_classify_incomplete_reasons_and_preserve_native_wire() {
    for (reason, expected) in [
        ("max_output_tokens", FinishReason::Length),
        ("max_tokens", FinishReason::Length),
        ("max_prompt_tokens", FinishReason::Length),
        ("max_time_limit", FinishReason::Length),
        ("content_filter", FinishReason::ContentFilter),
        ("future_reason", FinishReason::Other),
    ] {
        let created = serde_json::json!({"type":"response.created","response":{"id":"resp_short"}});
        let incomplete = serde_json::json!({
            "type":"response.incomplete",
            "response":{
                "id":"resp_short","status":"incomplete",
                "incomplete_details":{"reason":reason}
            }
        });
        let body = format!("data: {created}\n\ndata: {incomplete}\n\n");
        let events = GrokCanonicalDecoder::new("fallback")
            .push(body.as_bytes())
            .expect("incomplete response");
        let completed = events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .find_map(|fact| match fact {
                GatewayEvent::Completed(meta) => meta.finish_reason(),
                _ => None,
            });

        assert_eq!(completed, Some(expected), "{reason}");
        assert_eq!(wire_events(&events).last().unwrap().data(), &incomplete);
    }
}

#[test]
fn decoder_should_preserve_structured_failure_fields_and_hide_raw_debug() {
    for (event, expected_error_type) in [
        (
            serde_json::json!({"type":"response.failed","error":{"code":"rate_limit_exceeded","type":"server_error","message":"secret"}}),
            Some("server_error"),
        ),
        (
            serde_json::json!({"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","type":"provider_specific_error","message":"secret"}}}),
            Some("provider_specific_error"),
        ),
        (
            serde_json::json!({"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","message":"secret"}}}),
            None,
        ),
        (
            serde_json::json!({"type":"error","code":"rate_limit_exceeded","message":"secret"}),
            None,
        ),
    ] {
        let body = format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
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
        assert_eq!(visible.error_type(), expected_error_type);
        let raw: serde_json::Value =
            serde_json::from_str(error.raw_upstream_error().unwrap().as_str()).unwrap();
        assert_eq!(raw, event);
        assert!(!format!("{error:?}").contains("secret"));
    }
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
        error.diagnostic().unwrap().as_str(),
        "上游账号免费额度已用完"
    );
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
    let fields = error
        .upstream_failure_fields()
        .expect("original upstream fields");
    assert_eq!(fields.status, None);
    assert_eq!(fields.error_type, None);
    assert!(
        fields
            .message
            .as_deref()
            .unwrap()
            .contains("00000000-0000-0000-0000-000000000013")
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

    assert_eq!(error.upstream_failure_fields().unwrap().message, None);
    assert!(error.client_visible_upstream_error().is_none());
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("internal_error")
    );
}

#[test]
fn native_sse_preserves_unknown_malformed_ping_and_tool_frames_byte_for_byte() {
    let raw = concat!(
        "event: response.created\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"grok-4.7\"}}\r\n\r\n",
        ": keep-alive\n\n",
        "event: ping\ndata: {\"type\":\"ping\",\"x\":true}\n\n",
        "event: response.doom_loop_check\nid: native-id\nretry: 42\ndata: {\"type\":\"response.doom_loop_check\",\"unknown\": [1,2]}\n\n",
        "event: future\ndata: {broken json\n\n",
        "retry: not-a-number\ndata: no schema\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":7,\"sequence_number\":100,\"item\":{\"id\":\"fc1\",\"type\":\"function_call\",\"name\":\"xai_proxy_apply_patch\",\"call_id\":\"c1\",\"arguments\":\"{ \\\"n\\\":1.0 }\"}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[]}}\n\n",
        "data: [DONE]\n\n"
    );
    let mut decoder = GrokCanonicalDecoder::new("grok-4.7");
    let mut events = Vec::new();
    for chunk in raw.as_bytes().chunks(13) {
        events.extend(decoder.push(chunk).unwrap());
    }
    events.extend(decoder.finish().unwrap());
    let preserved: Vec<u8> = wire_events(&events)
        .into_iter()
        .flat_map(|wire| {
            assert_eq!(wire.protocol(), "openai");
            wire.raw_sse_frame().unwrap().to_vec()
        })
        .collect();
    assert_eq!(preserved, raw.as_bytes());
    let native = wire_events(&events)
        .into_iter()
        .find(|wire| wire.data().get("output_index") == Some(&serde_json::json!(7)))
        .unwrap();
    assert_eq!(native.data()["sequence_number"], 100);
    assert_eq!(native.data()["item"]["name"], "xai_proxy_apply_patch");
}
