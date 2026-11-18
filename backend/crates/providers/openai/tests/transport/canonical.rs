//! 验证 Codex SSE 转换保留协议事件并提取模型和用量事实

use gateway_core::event::{
    ContentKind, EventSequenceValidator, FinishReason, GatewayEvent, ProviderEvent,
};

use provider_openai::transport::canonical::{
    CodexCanonicalDecoder, CodexCanonicalError, CodexCanonicalFailure, CodexCanonicalOutcome,
};
use provider_openai::transport::protocol::websocket::websocket_event_to_sse_frame;
use serde_json::json;

#[test]
fn diagnostic_usage_survives_large_terminal_events_without_changing_wire() {
    for websocket in [false, true] {
        for (cached, expected_state, expected_value) in [
            (None, "missing", None),
            (Some(json!(0)), "value", Some(0)),
            (Some(json!(180_000)), "value", Some(180_000)),
            (Some(json!("private-invalid-token-value")), "invalid", None),
        ] {
            let trace = gateway_core::diagnostics::TraceContext::new("req_usage_evidence");
            let mut decoder = CodexCanonicalDecoder::new("gpt-5.4")
                .with_trace(trace.clone())
                .with_raw_sse_passthrough();
            let mut value = json!({"type":"response.completed", "response":{
                "id":"resp_evidence", "output":[{"type":"message", "id":"msg_evidence", "role":"assistant", "content":[{"type":"output_text", "text":"private-output".repeat(20_000)}]}],
                "usage":{"input_tokens":200_000, "output_tokens":50, "input_tokens_details":{}}
            }});
            if let Some(cached) = cached {
                value["response"]["usage"]["input_tokens_details"]["cached_tokens"] = cached;
            }
            let raw = value.to_string();
            let frame = if websocket {
                websocket_event_to_sse_frame(&raw).unwrap()
            } else {
                format!("data: {raw}\n\n")
            };
            let events = decoder
                .push(frame.as_bytes())
                .expect("large terminal event");
            assert!(
                events
                    .iter()
                    .any(|event| event.wire_event().is_some_and(|wire| wire.data() == &value))
            );
            let snapshot = trace.snapshot().unwrap();
            let evidence = &snapshot["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["stage"] == "provider.response.usage")
                .unwrap()["data"];
            let cached = &evidence["fields"]["cachedInputTokens"][0];
            assert_eq!(cached["state"], expected_state);
            assert_eq!(cached["value"].as_u64(), expected_value);
            assert_eq!(evidence["parsedUsage"]["inputTokens"], 200_000);
            assert_eq!(
                evidence["parsedUsage"]["cachedInputTokens"],
                expected_value.unwrap_or(0)
            );
            assert!(!snapshot.to_string().contains("private-output"));
            assert!(!snapshot.to_string().contains("private-invalid-token-value"));
        }
    }
}

#[test]
fn diagnostic_parse_failure_keeps_raw_passthrough_without_persisting_plaintext() {
    let trace = gateway_core::diagnostics::TraceContext::new("req_parse_evidence");
    let mut decoder = CodexCanonicalDecoder::new("model")
        .with_trace(trace.clone())
        .with_raw_sse_passthrough();
    let events = decoder
        .push(b"data: private-invalid-json\n\n")
        .expect("raw invalid event remains deliverable");
    assert!(events.iter().any(|event| event.wire_event().is_some()));
    let snapshot = trace.snapshot().unwrap();
    assert_eq!(
        snapshot["events"][0]["stage"],
        "provider.response.parse_gap"
    );
    assert!(!snapshot.to_string().contains("private-invalid-json"));
}

const METADATA_PREFIX_FIXTURE: &str = include_str!("fixtures/metadata_only_prefix.sse");

#[test]
fn official_response_duration_uses_only_the_completed_response_on_both_transports() {
    for websocket in [false, true] {
        for (created_at, completed_at, expected) in [
            (json!(1_791_441_115), json!(1_791_441_122), Some(7_000)),
            (json!(100.25), json!(101.5), Some(1_250)),
            (json!(100), json!(100), None),
            (json!(101), json!(100), None),
            (json!(-1), json!(100), None),
            (json!(null), json!(100), None),
            (json!(100), json!(null), None),
            (json!("100"), json!(101), None),
            (json!(0), json!(1e30), None),
        ] {
            let mut decoder = CodexCanonicalDecoder::new("gpt-5.4").with_raw_sse_passthrough();
            // 前导响应的时间不能用于补齐完成响应缺失的时间
            decoder.push(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_time\",\"created_at\":1}}\n\n").expect("created response");
            assert_eq!(decoder.upstream_response_ms(), None);
            let completed = json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_time", "status": "completed", "output": [],
                    "created_at": created_at, "completed_at": completed_at,
                    "usage": { "input_tokens": 1, "output_tokens": 112, "total_tokens": 113 },
                },
            });
            let raw = completed.to_string();
            let frame = if websocket {
                websocket_event_to_sse_frame(&raw).expect("WS completion")
            } else {
                format!("data: {raw}\n\n")
            };
            let split = frame.len() / 2;
            decoder
                .push(&frame.as_bytes()[..split])
                .expect("first chunk");
            let events = decoder
                .push(&frame.as_bytes()[split..])
                .expect("completion");
            assert_eq!(decoder.upstream_response_ms(), expected);
            assert!(events.iter().any(|event| {
                event
                    .wire_event()
                    .is_some_and(|wire| wire.data() == &completed)
            }));
            assert!(canonical_facts(&events).iter().any(|event| matches!(event, GatewayEvent::Usage(usage) if usage.output_tokens == Some(112))));
        }
    }
}

#[test]
fn official_response_duration_does_not_cross_response_identity_or_incomplete_boundary() {
    for (event_type, response_id, created) in [
        ("response.completed", "resp_other", true),
        ("response.completed", "resp_time", false),
        ("response.incomplete", "resp_time", true),
    ] {
        let mut decoder = CodexCanonicalDecoder::new("gpt-5.4");
        if created {
            decoder.push(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_time\"}}\n\n").expect("created response");
        }
        let completed = json!({
            "type": event_type,
            "response": {
                "id": response_id, "created_at": 100, "completed_at": 107,
                "usage": { "input_tokens": 1, "output_tokens": 112, "total_tokens": 113 },
            },
        });
        let events = decoder
            .push(format!("data: {completed}\n\n").as_bytes())
            .expect("terminal response");
        assert_eq!(decoder.upstream_response_ms(), None);
        assert!(events.iter().any(|event| {
            event
                .wire_event()
                .is_some_and(|wire| wire.data() == &completed)
        }));
    }
}

#[test]
fn first_response_observation_uses_nonempty_data_before_json_or_event_classification() {
    for data in [
        r#"{"type":"response.created","response":{"id":"resp_first"}}"#,
        r#"{"type":"response.failed","response":{"error":{"message":"failed"}}}"#,
        r#"{"type":"future.event"}"#,
        "not-json",
    ] {
        for websocket in [false, true] {
            let mut decoder = CodexCanonicalDecoder::new("fallback").with_raw_sse_passthrough();
            if websocket {
                let _ = decoder.push_websocket(data);
            } else {
                let _ = decoder
                    .push(b": ping\r\nevent: response.created\r\ndata: \t\r\n\r\ndata: [DONE]\n\n");
                assert!(!decoder.take_timing_signals().response_data);
                let frame = format!("data: {data}\r\n\r\n");
                let split = frame.len() / 2;
                let _ = decoder.push(&frame.as_bytes()[..split]);
                assert!(!decoder.take_timing_signals().response_data);
                let _ = decoder.push(&frame.as_bytes()[split..]);
            }
            let signals = decoder.take_timing_signals();
            assert!(signals.response_data, "websocket={websocket}, data={data}");
            assert!(!signals.semantic_output);
            assert!(!signals.reasoning_output);
            assert!(!signals.text_output);
        }
    }
}

#[test]
fn official_timing_frames_start_first_response_without_content() {
    let mut decoder = CodexCanonicalDecoder::new("gpt-5.4");
    let events = decoder.push(b"data: {\"type\":\"responsesapi.websocket_timing\",\"timing_metrics\":{\"total_turn_time_s\":20}}\n\n").expect("timing frame");
    assert!(!events.is_empty());
    assert!(decoder.take_timing_signals().response_data);
    assert_eq!(decoder.upstream_response_ms(), None);
}

#[test]
fn decoder_should_not_forward_codex_rate_limit_metadata_fixture_as_openai_wire() {
    let events = CodexCanonicalDecoder::new("fallback")
        .with_raw_sse_passthrough()
        .push(METADATA_PREFIX_FIXTURE.as_bytes())
        .expect("metadata fixture should remain open-world");
    let wire_types = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .filter_map(|wire| wire.event_type())
        .collect::<Vec<_>>();

    assert_eq!(wire_types, vec!["response.created", "response.metadata"]);
    assert!(matches!(
        canonical_facts(&events).as_slice(),
        [GatewayEvent::Started(_)]
    ));
}

#[test]
fn raw_sse_passthrough_should_observe_rate_limit_response_without_forwarding_it() {
    let frame = concat!(
        "event: codex.rate_limits\n",
        "data: {\"type\":\"codex.rate_limits\",\"rate_limits\":{\"primary\":{\"used_percent\":90}}}\n\n",
    );
    let mut decoder = CodexCanonicalDecoder::new("fallback").with_raw_sse_passthrough();

    let events = decoder
        .push(frame.as_bytes())
        .expect("quota control frame should be accepted");
    let signals = decoder.take_timing_signals();

    assert!(events.is_empty());
    assert!(!signals.protocol_progress);
    assert!(signals.response_data);
    assert!(!signals.semantic_output);
}

#[test]
fn decoder_should_preserve_empty_opaque_response_id_in_canonical_facts() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"\",\"model\":\"gpt-test\",\"status\":\"completed\",\"output\":[]}}\n\n",
    );

    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("empty opaque response ID remains representable");
    let facts = canonical_facts(&events);

    assert!(matches!(
        facts.as_slice(),
        [GatewayEvent::Started(started), GatewayEvent::Completed(completed)]
            if started.response_id().is_empty() && completed.response_id().is_empty()
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
    // data 段必须与上游原文逐字节一致，不得经 serde 往返改写数值/精度
    // `1e3` 若被重序列化会变成 `1000.0`——用它作为字节改写的探针
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
fn raw_sse_passthrough_should_preserve_long_event_metadata() {
    let event_name = "x".repeat(300);
    let frame = format!("event: {event_name}\ndata: {{\"type\":\"noise\",\"marker\":1}}\n\n");

    let events = CodexCanonicalDecoder::new("fallback")
        .with_raw_sse_passthrough()
        .push(frame.as_bytes())
        .expect("opaque event metadata must not abort transparent transport");
    let wire = events[0].wire_event().expect("wire event");

    assert_eq!(
        wire.raw_sse_frame().map(AsRef::as_ref),
        Some(frame.as_bytes())
    );
    assert!(wire.has_json_data());
    assert_eq!(wire.event_type(), Some(event_name.as_str()));
}

#[test]
fn decoder_should_preserve_long_event_metadata_without_raw_frame() {
    let event_name = "y".repeat(300);
    let frame = format!("event: {event_name}\ndata: {{\"type\":\"noise\",\"marker\":1}}\n\n");

    let events = CodexCanonicalDecoder::new("fallback")
        .push(frame.as_bytes())
        .expect("opaque event metadata must not drop the event");
    let wire = events[0].wire_event().expect("wire event");

    assert_eq!(wire.event_type(), Some(event_name.as_str()));
    assert!(wire.has_json_data());
    assert_eq!(wire.data()["marker"], serde_json::json!(1));
}

#[test]
fn decoder_should_bill_the_sent_model_independently_of_the_response_model() {
    for (sent, returned, expected_cost) in [
        ("gpt-5.6-sol", Some("gpt-6-sol"), Some(6_875_000)),
        ("gpt-5.6-sol", Some("gpt-6-astra"), Some(6_875_000)),
        ("gpt-5.6-sol", Some("gpt-5.6-sol"), Some(6_875_000)),
        ("gpt-5.6-sol", None, Some(6_875_000)),
        ("gpt-6-sol", Some("gpt-5.6-sol"), Some(2_550_000)),
        ("gpt-6-luna", Some("gpt-6-astra"), Some(127_500)),
    ] {
        let created =
            json!({"type":"response.created","response":{"id":"resp_model_cost","model":returned}});
        let completed = json!({
            "type":"response.completed",
            "response":{
                "id":"resp_model_cost","model":returned,"status":"completed","output":[],
                "usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":25},"total_tokens":110}
            }
        });
        let body = format!("data: {created}\n\ndata: {completed}\n\n");
        let mut decoder = CodexCanonicalDecoder::new(sent).with_raw_sse_passthrough();
        let events = decoder.push(body.as_bytes()).expect("model cost response");
        let cost = canonical_facts(&events)
            .into_iter()
            .find_map(|event| match event {
                GatewayEvent::CalculatedCost(cost) => Some(cost.total().amount().scaled()),
                _ => None,
            });
        assert_eq!(cost, expected_cost, "{sent} -> {returned:?}");
        assert!(canonical_facts(&events).into_iter().any(|event| matches!(
            event,
            GatewayEvent::Completed(meta) if meta.model() == Some(returned.unwrap_or(sent))
        )));
        let raw: Vec<u8> = events
            .iter()
            .filter_map(|event| event.wire_event().and_then(|wire| wire.raw_sse_frame()))
            .flat_map(|frame| frame.iter().copied())
            .collect();
        assert_eq!(raw, body.as_bytes());
    }
}

#[test]
fn decoder_should_emit_calculated_cost_for_complete_known_model_usage() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cost\",\"model\":\"gpt-5.4\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cost\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":25,\"cache_write_tokens\":0},\"total_tokens\":110}}}\n\n",
    );
    let events = CodexCanonicalDecoder::new("gpt-5.4")
        .push(body.as_bytes())
        .expect("canonical priced response");

    assert!(canonical_facts(&events).into_iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(cost)
            if cost.total().amount().scaled() == 3_437_500
    )));
}

#[test]
fn decoder_should_bill_requested_service_tier_despite_default_response() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fast_cost\",\"model\":\"gpt-5.4\",\"service_tier\":\"default\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fast_cost\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":25,\"cache_write_tokens\":0},\"total_tokens\":110}}}\n\n",
    );
    let mut decoder =
        CodexCanonicalDecoder::new("gpt-5.4").with_requested_service_tier(Some("priority"));
    let events = decoder
        .push(body.as_bytes())
        .expect("canonical priority response");

    assert_eq!(decoder.response_service_tier(), Some("default"));
    assert!(canonical_facts(&events).into_iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(cost)
            if cost.total().amount().scaled() == 6_875_000
    )));
}

#[test]
fn decoder_should_bill_standard_when_request_omits_service_tier() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_response_tier_cost\",\"model\":\"gpt-5.4\",\"service_tier\":\"priority\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_response_tier_cost\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":25,\"cache_write_tokens\":0},\"total_tokens\":110}}}\n\n",
    );
    let mut decoder = CodexCanonicalDecoder::new("gpt-5.4");
    let events = decoder
        .push(body.as_bytes())
        .expect("canonical response-tier priority response");

    assert_eq!(decoder.response_service_tier(), Some("priority"));
    assert!(canonical_facts(&events).into_iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(cost)
            if cost.total().amount().scaled() == 3_437_500
    )));
}

#[test]
fn decoder_should_add_standard_web_search_call_cost() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_search_cost\",\"model\":\"gpt-5.4\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_search_cost\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"status\":\"completed\",\"action\":{\"type\":\"search\"}}],\"usage\":{\"input_tokens\":0,\"output_tokens\":0,\"total_tokens\":0}}}\n\n",
    );
    let tools = vec![json!({ "type": "web_search" })];
    let events = CodexCanonicalDecoder::new("gpt-5.4")
        .with_request_tool_pricing("gpt-5.4", Some(&tools))
        .push(body.as_bytes())
        .expect("canonical web search response");

    assert!(canonical_facts(&events).into_iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(cost)
            if cost.total().amount().scaled() == 100_000_000
    )));
}

#[test]
fn decoder_should_use_non_reasoning_preview_web_search_price() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_preview_search_cost\",\"model\":\"gpt-4o\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_preview_search_cost\",\"model\":\"gpt-4o\",\"status\":\"completed\",\"output\":[{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"status\":\"completed\",\"action\":{\"type\":\"search\"}}],\"usage\":{\"input_tokens\":0,\"output_tokens\":0,\"total_tokens\":0}}}\n\n",
    );
    let tools = vec![json!({ "type": "web_search_preview" })];
    let events = CodexCanonicalDecoder::new("gpt-4o")
        .with_request_tool_pricing("gpt-4o", Some(&tools))
        .push(body.as_bytes())
        .expect("canonical preview web search response");

    assert!(canonical_facts(&events).into_iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(cost)
            if cost.total().amount().scaled() == 250_000_000
    )));
}

#[test]
fn gpt_6_decoders_should_use_reasoning_preview_web_search_price() {
    let tools = vec![json!({ "type": "web_search_preview" })];
    for model in [
        "gpt-6.2-astra",
        "gpt-6.2-sol",
        "gpt-6-astra",
        "gpt-6.1-sol",
        "gpt-6.1-terra",
        "gpt-6-sol",
        "gpt-6-luna",
    ] {
        let created = json!({
            "type":"response.created",
            "response":{"id":"resp_preview_search_cost","model":model}
        });
        let completed = json!({
            "type":"response.completed",
            "response":{
                "id":"resp_preview_search_cost","model":model,"status":"completed",
                "output":[{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search"}}],
                "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}
            }
        });
        let body = format!("data: {created}\n\ndata: {completed}\n\n");
        let events = CodexCanonicalDecoder::new(model)
            .with_request_tool_pricing(model, Some(&tools))
            .push(body.as_bytes())
            .expect("canonical preview web search response");

        assert!(
            canonical_facts(&events).into_iter().any(|event| matches!(
                event,
                GatewayEvent::CalculatedCost(cost)
                    if cost.total().amount().scaled() == 100_000_000
            )),
            "{model}"
        );
    }
}

#[test]
fn decoder_should_fail_closed_for_fixed_block_web_search_content() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fixed_search_cost\",\"model\":\"gpt-4o-mini\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fixed_search_cost\",\"model\":\"gpt-4o-mini\",\"status\":\"completed\",\"output\":[{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"status\":\"completed\",\"action\":{\"type\":\"search\"}}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
    );
    let tools = vec![json!({ "type": "web_search" })];
    let events = CodexCanonicalDecoder::new("gpt-4o-mini")
        .with_request_tool_pricing("gpt-4o-mini", Some(&tools))
        .push(body.as_bytes())
        .expect("canonical fixed-block search response");

    assert!(
        canonical_facts(&events)
            .into_iter()
            .all(|event| !matches!(event, GatewayEvent::CalculatedCost(_)))
    );
}

#[test]
fn websocket_decoder_should_bill_requested_service_tier_despite_default_response() {
    let created = websocket_event_to_sse_frame(
        r#"{"type":"response.created","response":{"id":"resp_ws_fast_cost","model":"gpt-5.4","service_tier":"default"}}"#,
    )
    .expect("created frame");
    let completed = websocket_event_to_sse_frame(
        r#"{"type":"response.completed","response":{"id":"resp_ws_fast_cost","model":"gpt-5.4","status":"completed","usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":25,"cache_write_tokens":0},"total_tokens":110}}}"#,
    )
    .expect("completed frame");
    let mut decoder = CodexCanonicalDecoder::new("gpt-5.4")
        .with_requested_service_tier(Some("priority"))
        .with_raw_sse_passthrough();
    let events = decoder
        .push(format!("{created}{completed}").as_bytes())
        .expect("canonical priority WebSocket response");

    assert_eq!(decoder.response_service_tier(), Some("default"));
    assert!(canonical_facts(&events).into_iter().any(|event| matches!(
        event,
        GatewayEvent::CalculatedCost(cost)
            if cost.total().amount().scaled() == 6_875_000
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
fn reasoning_parts_share_item_registration_and_preserve_wire() {
    for websocket in [false, true] {
        for (family, index_field, part_type) in [
            ("reasoning_summary", "summary_index", "summary_text"),
            ("reasoning", "content_index", "reasoning_text"),
        ] {
            let trace = gateway_core::diagnostics::TraceContext::new("req_reasoning_parts");
            let mut decoder = CodexCanonicalDecoder::new("test-model")
                .with_trace(trace.clone())
                .with_raw_sse_passthrough();
            let mut frames = vec![
                json!({"type":"response.created", "response":{"id":"resp_parts", "model":"test-model"}}),
                json!({"type":"response.output_item.added", "output_index":0,
                    "item":{"id":"rs_parts", "type":"reasoning", "summary":[]}}),
            ];
            for (index, text) in [(0, "first"), (1, "second")] {
                frames.extend([
                    json!({"type":format!("response.{family}_part.added"), "item_id":"rs_parts",
                        "output_index":0, (index_field):index, "part":{"type":part_type, "text":""}}),
                    json!({"type":format!("response.{family}_text.delta"), "item_id":"rs_parts",
                        "output_index":0, (index_field):index, "delta":text}),
                    json!({"type":format!("response.{family}_part.done"), "item_id":"rs_parts",
                        "output_index":0, (index_field):index, "part":{"type":part_type, "text":text}}),
                ]);
            }
            frames.push(json!({"type":"response.completed", "response":{
                "id":"resp_parts", "model":"test-model", "status":"completed"}}));
            let mut all_events = Vec::new();
            for frame in &frames {
                let raw = if websocket {
                    frame.to_string()
                } else {
                    format!(
                        "event: {}\ndata: {frame}\n\n",
                        frame["type"].as_str().unwrap()
                    )
                };
                let events = if websocket {
                    decoder.push_websocket(&raw)
                } else {
                    decoder.push(raw.as_bytes())
                }
                .expect("reasoning event remains deliverable");
                let wire = events
                    .iter()
                    .filter_map(ProviderEvent::wire_event)
                    .collect::<Vec<_>>();
                assert_eq!(wire.len(), 1);
                let delivered = if websocket {
                    wire[0].raw_websocket_message().map(str::as_bytes)
                } else {
                    wire[0].raw_sse_frame().map(AsRef::as_ref)
                };
                assert_eq!(delivered, Some(raw.as_bytes()));
                if frame["type"] == format!("response.{family}_part.added") {
                    let added = canonical_facts(&events)
                        .into_iter()
                        .filter_map(|fact| match fact {
                            GatewayEvent::ContentAdded(item) => Some((item.index(), item.kind())),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    let expected = if frame[index_field] == 0 {
                        vec![]
                    } else {
                        vec![(1, ContentKind::Reasoning)]
                    };
                    assert_eq!(added, expected, "each part is registered before its delta");
                }
                all_events.extend(events);
            }
            let facts = canonical_facts(&all_events);
            let mut validator = EventSequenceValidator::new();
            for fact in &facts {
                validator
                    .observe(fact)
                    .expect("valid canonical event sequence");
            }
            validator.finish().expect("completed canonical sequence");
            let reasoning = facts
                .iter()
                .filter_map(|fact| match fact {
                    GatewayEvent::ReasoningDelta(delta) => {
                        Some((delta.content_index, delta.text.as_str()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(reasoning, [(0, "first"), (1, "second")]);
            assert!(
                trace.snapshot().unwrap()["stageCounts"]["provider.response.parse_gap"].is_null()
            );
        }
    }
}

#[test]
fn reasoning_part_type_conflict_preserves_registered_content_and_wire() {
    let trace = gateway_core::diagnostics::TraceContext::new("req_part_conflict");
    let mut decoder = CodexCanonicalDecoder::new("test-model").with_trace(trace.clone());
    let frames = [
        json!({"type":"response.created", "response":{"id":"resp_conflict", "model":"test-model"}}),
        json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"reasoning"}}),
        json!({"type":"response.content_part.added", "output_index":0, "content_index":0,
            "part":{"type":"output_text", "text":""}}),
        json!({"type":"response.reasoning_summary_text.delta", "output_index":0, "summary_index":0, "delta":"reasoning"}),
    ];
    let mut all_events = Vec::new();
    for frame in &frames {
        let raw = frame.to_string();
        let events = decoder
            .push_websocket(&raw)
            .expect("wire survives projection conflict");
        let wire = events
            .iter()
            .filter_map(ProviderEvent::wire_event)
            .collect::<Vec<_>>();
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].raw_websocket_message(), Some(raw.as_str()));
        all_events.extend(events);
    }
    let facts = canonical_facts(&all_events);
    assert!(
        matches!(facts.as_slice(), [GatewayEvent::Started(_), GatewayEvent::ContentAdded(item), GatewayEvent::ReasoningDelta(delta)]
        if item.kind() == ContentKind::Reasoning && delta.text == "reasoning")
    );
    assert_eq!(
        trace.snapshot().unwrap()["stageCounts"]["provider.response.parse_gap"],
        1
    );
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
fn decoder_timing_signals_distinguish_tool_item_start_from_argument_output() {
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
fn decoder_should_not_understate_cost_when_image_tool_price_is_ambiguous() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_image_cost\",\"model\":\"gpt-5.4\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_image_cost\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"total_tokens\":17},\"tool_usage\":{\"image_gen\":{\"input_tokens\":31,\"output_tokens\":9}}}}\n\n",
    );
    let events = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect("canonical image cost response");

    assert!(
        canonical_facts(&events)
            .into_iter()
            .all(|event| !matches!(event, GatewayEvent::CalculatedCost(_)))
    );
}

#[test]
fn decoder_should_drop_rate_limit_control_and_preserve_metadata_lifecycle_events() {
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
        .with_raw_sse_passthrough()
        .push(body.as_bytes())
        .expect("official Codex metadata lifecycle events");
    let canonical = canonical_facts(&events);
    let wire_types = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .filter_map(|wire| wire.event_type())
        .collect::<Vec<_>>();

    assert_eq!(
        wire_types,
        vec![
            "response.metadata",
            "response.in_progress",
            "response.output_text.delta",
            "response.completed"
        ]
    );
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
fn capacity_failure_preserves_original_wire_and_diagnostics() {
    for path in ["error", "response.failed"] {
        let data = if path == "error" {
            r#"{"type":"error","error":{"code":"slow_down","message":"busy","extra":123456789012345678901234567890},"extension":true}"#
        } else {
            r#"{"type":"response.failed","response":{"id":"resp_capacity","error":{"code":"server_is_overloaded","message":"busy","extra":123456789012345678901234567890}},"extension":true}"#
        };
        let raw = format!("event: {path}\r\nid: upstream-id\r\nretry: 123\r\ndata: {data}\r\n\r\n");
        let failure = CodexCanonicalDecoder::new("fallback")
            .with_raw_sse_passthrough()
            .push(raw.as_bytes())
            .expect_err("capacity failure");
        let wire = failure.events()[0].wire_event().expect("client wire");
        let expected: serde_json::Value = serde_json::from_str(data).expect("original JSON");
        assert_eq!(wire.data(), &expected);
        assert_eq!(wire.sse_id(), Some("upstream-id"));
        assert_eq!(wire.sse_retry(), Some(123));
        assert_eq!(
            wire.raw_sse_frame().map(AsRef::as_ref),
            Some(raw.as_bytes())
        );
        let CodexCanonicalError::Upstream(upstream) = failure.error() else {
            panic!("typed failure")
        };
        assert_eq!(upstream.raw_body(), data);
        assert_ne!(upstream.upstream_code.as_deref(), Some("server_error"));
    }
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
                && metadata.model() == Some("fallback-model")
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
    let raw_error = format!(
        "{{\"response\":{{\"id\":\"resp_failed\",\"status\":\"failed\",\"error\":{{\"code\":\"{code}\",\"message\":\"{marker}\"}}}}}}"
    );
    let body = format!("event: response.failed\ndata: {raw_error}\n\n");
    let failure = CodexCanonicalDecoder::new("fallback")
        .push(body.as_bytes())
        .expect_err("failed event must become a typed error");

    let CodexCanonicalError::Upstream(upstream) = failure.error() else {
        panic!("response.failed must preserve its typed upstream failure");
    };
    assert_eq!(upstream.upstream_code.as_deref(), Some(code));
    assert_eq!(upstream.raw_body(), raw_error);
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

fn pricing_response_cost(
    requested: Option<&str>,
    actual: Option<&str>,
    output: serde_json::Value,
    tools: &[serde_json::Value],
) -> Option<u128> {
    let mut response = json!({
        "id": "resp_pricing", "model": "gpt-6-astra", "status": "completed",
        "output": output, "usage": {"input_tokens": 10_000, "output_tokens": 0, "total_tokens": 10_000}
    });
    if let Some(tier) = actual {
        response["service_tier"] = json!(tier);
    }
    let created = json!({"type": "response.created", "response": {"id": "resp_pricing", "model": "gpt-6-astra"}});
    let completed = json!({"type": "response.completed", "response": response});
    let body = format!(
        "event: response.created\ndata: {created}\n\nevent: response.completed\ndata: {completed}\n\n"
    );
    let events = CodexCanonicalDecoder::new("gpt-6-astra")
        .with_requested_service_tier(requested)
        .with_request_tool_pricing("gpt-6-astra", Some(tools))
        .with_raw_sse_passthrough()
        .push(body.as_bytes())
        .expect("计费不确定时仍须正常透传响应");
    assert!(
        canonical_facts(&events)
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
    );
    canonical_facts(&events)
        .into_iter()
        .find_map(|event| match event {
            GatewayEvent::CalculatedCost(cost) => Some(cost.total().amount().scaled()),
            _ => None,
        })
}

#[test]
fn billing_should_follow_requested_tier_independently_of_response_tier() {
    for (requested, actual, expected) in [
        (Some("priority"), Some("default"), Some(2_000_000_000)),
        (Some("fast"), Some("default"), Some(2_000_000_000)),
        (Some("ultrafast"), Some("default"), Some(6_000_000_000)),
        (Some("ultrafast"), None, Some(6_000_000_000)),
        (Some("auto"), Some("fast"), None),
        (Some("default"), Some("flex"), Some(1_000_000_000)),
        (Some("flex"), Some("priority"), Some(500_000_000)),
        (Some("fast"), None, Some(2_000_000_000)),
        (Some("auto"), None, None),
        (Some("default"), Some("future"), Some(1_000_000_000)),
        (Some("future"), Some("default"), None),
        (None, Some("priority"), Some(1_000_000_000)),
        (None, Some("ultrafast"), Some(1_000_000_000)),
        (None, None, Some(1_000_000_000)),
    ] {
        assert_eq!(
            pricing_response_cost(requested, actual, json!([]), &[]),
            expected,
            "{requested:?} -> {actual:?}"
        );
    }
}

#[test]
fn billing_should_add_file_and_web_search_fees_without_tier_markup() {
    let output = json!([
        {"type": "file_search_call", "id": "fs_1", "status": "completed"},
        {"type": "file_search_call", "id": "fs_2", "status": "completed"},
        {"type": "web_search_call", "id": "ws_1", "status": "completed", "action": {"type": "search"}}
    ]);
    for (tier, expected) in [
        ("default", 1_150_000_000),
        ("fast", 2_150_000_000),
        ("ultrafast", 6_150_000_000),
    ] {
        assert_eq!(
            pricing_response_cost(
                Some(tier),
                Some("default"),
                output.clone(),
                &[json!({"type": "web_search"})]
            ),
            Some(expected)
        );
    }
}

#[test]
fn billing_should_not_emit_partial_totals_for_unpriced_tool_outputs() {
    for output in [
        json!([{"type": "code_interpreter_call", "id": "ci_1", "status": "completed"}]),
        json!([{"type": "shell_call", "id": "sh_1", "status": "completed"}]),
        json!([{"type": "image_generation_call", "id": "ig_1", "status": "completed"}]),
        json!([{"type": "future_paid_tool_call", "id": "tool_1"}]),
        json!([{"type": "file_search_call", "id": "fs_1", "status": "failed"}]),
        json!({"malformed": true}),
    ] {
        assert_eq!(
            pricing_response_cost(None, None, output.clone(), &[]),
            None,
            "{output}"
        );
    }
}
