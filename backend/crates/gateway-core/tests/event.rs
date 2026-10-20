//! 验证 Provider 事件的协议保留、敏感值保护与事件序列约束

use bytes::Bytes;
use gateway_core::event::{
    ContentItem, ContentKind, EventSequenceError, EventSequenceValidator, GatewayEvent,
    ProtocolWireEvent, ProviderEvent, ProviderResponseHeader, ProviderResponseMetadata,
    ProviderResponseObservation, ProviderResponseTimings, ReasoningDelta, ResponseMeta, TextDelta,
    ToolCallDelta,
};
use gateway_core::upstream::UpstreamTransport;
use serde_json::json;

#[test]
fn provider_event_should_distinguish_client_delivery_from_observation() {
    let wire = ProviderEvent::wire(
        ProtocolWireEvent::json("openai", None, json!({"type": "response.created"}))
            .expect("wire event"),
    );
    let observation = ProviderEvent::observation(ProviderResponseObservation::new(
        UpstreamTransport::new("http_sse").expect("transport"),
    ));
    assert!(wire.has_client_event());
    assert!(!observation.has_client_event());
}

#[test]
fn provider_response_header_should_preserve_opaque_name_and_bytes_without_debug_disclosure() {
    let name = format!("x-future-{}\0", "n".repeat(512));
    let value = Bytes::from_static(b"\xffopaque-response-header-secret");
    let header = ProviderResponseHeader::new(name.clone(), value.clone());

    assert_eq!(header.name(), name);
    assert_eq!(header.value(), &value);
    let rendered = format!("{header:?}");
    assert!(!rendered.contains(&name));
    assert!(!rendered.contains("opaque-response-header-secret"));
}

#[test]
fn invalid_observed_service_tier_should_be_ignored() {
    let observation =
        ProviderResponseObservation::new(UpstreamTransport::new("http_sse").expect("transport"))
            .with_service_tier_if_valid(format!("priority\0{}", "x".repeat(128)));

    assert_eq!(observation.service_tier(), None);
}

#[test]
fn protocol_wire_event_should_preserve_sse_metadata_without_exposing_id_in_debug() {
    let wire = ProtocolWireEvent::json_with_sse_metadata(
        "openai",
        Some("response.created".to_owned()),
        json!({"type": "response.created"}),
        Some("upstream-event-id".to_owned()),
        Some(1_500),
    )
    .expect("wire event");

    assert_eq!(wire.sse_id(), Some("upstream-event-id"));
    assert_eq!(wire.sse_retry(), Some(1_500));
    assert!(!format!("{wire:?}").contains("upstream-event-id"));
}

#[test]
fn protocol_wire_event_should_preserve_opaque_sse_metadata() {
    let event_type = format!("\0{}", "event".repeat(80));
    let sse_id = "id\0with\r\ncontrols".to_owned();
    let wire = ProtocolWireEvent::json_with_sse_metadata(
        "openai",
        Some(event_type.clone()),
        json!({"type": "future.event"}),
        Some(sse_id.clone()),
        None,
    )
    .expect("internal protocol name is valid");

    assert_eq!(wire.event_type(), Some(event_type.as_str()));
    assert_eq!(wire.sse_id(), Some(sse_id.as_str()));
}

#[test]
fn protocol_wire_event_should_preserve_raw_json_body_without_parsing_it() {
    let raw = Bytes::from_static(
        br#"{ "created": 1, "data": [{"b64_json":"AAAA"}], "future": 9007199254740993 }"#,
    );
    let wire = ProtocolWireEvent::raw_json("openai", raw.clone()).expect("raw JSON event");

    assert_eq!(wire.raw_json_body(), Some(&raw));
    assert_eq!(wire.into_raw_json_body(), Some(raw));
}

#[test]
fn protocol_wire_event_should_preserve_raw_http_body_without_json_encoding() {
    let raw = Bytes::from_static(b"\0opaque-http-body\xff");
    let wire =
        ProtocolWireEvent::raw_http_body("provider-http", raw.clone()).expect("raw HTTP event");

    assert_eq!(wire.raw_http_body_bytes(), Some(&raw));
    assert_eq!(wire.into_raw_http_body(), Some(raw));
}

#[test]
fn validator_should_accept_started_content_delta_completed_sequence() {
    let mut validator = EventSequenceValidator::new();
    let events = [
        GatewayEvent::Started(ResponseMeta::new("resp_1", "smart-code")),
        GatewayEvent::ContentAdded(ContentItem::new(0, ContentKind::Text)),
        GatewayEvent::TextDelta(TextDelta {
            content_index: 0,
            text: "hello".to_owned(),
        }),
        GatewayEvent::Completed(ResponseMeta::new("resp_1", "smart-code")),
    ];

    for event in &events {
        validator
            .observe(event)
            .expect("test sequence is canonical");
    }

    assert_eq!(validator.finish(), Ok(()));
}

#[test]
fn validator_should_reject_delta_without_content_added() {
    let mut validator = EventSequenceValidator::new();
    validator
        .observe(&GatewayEvent::Started(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("started is valid");

    let error = validator
        .observe(&GatewayEvent::TextDelta(TextDelta {
            content_index: 0,
            text: "hello".to_owned(),
        }))
        .expect_err("delta must reference declared content");

    assert_eq!(error, EventSequenceError::InvalidDeltaTarget { index: 0 });
}

#[test]
fn validator_should_reject_empty_stream() {
    let validator = EventSequenceValidator::new();

    assert_eq!(validator.finish(), Err(EventSequenceError::MissingStarted));
}

#[test]
fn validator_should_reject_event_before_started() {
    let mut validator = EventSequenceValidator::new();

    let error = validator
        .observe(&GatewayEvent::ContentAdded(ContentItem::new(
            0,
            ContentKind::Text,
        )))
        .expect_err("content cannot precede Started");

    assert_eq!(error, EventSequenceError::MissingStarted);
}

#[test]
fn validator_should_reject_duplicate_started() {
    let mut validator = EventSequenceValidator::new();
    let started = GatewayEvent::Started(ResponseMeta::new("resp_1", "smart-code"));
    validator.observe(&started).expect("first Started is valid");

    let error = validator
        .observe(&started)
        .expect_err("Started may only appear once");

    assert_eq!(error, EventSequenceError::DuplicateStarted);
}

#[test]
fn validator_should_reject_duplicate_content_index() {
    let mut validator = EventSequenceValidator::new();
    validator
        .observe(&GatewayEvent::Started(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("Started is valid");
    let content = GatewayEvent::ContentAdded(ContentItem::new(3, ContentKind::Text));
    validator.observe(&content).expect("first content is valid");

    let error = validator
        .observe(&content)
        .expect_err("content indices must be unique");

    assert_eq!(error, EventSequenceError::DuplicateContent { index: 3 });
}

#[test]
fn validator_should_reject_text_delta_for_reasoning_content() {
    let mut validator = EventSequenceValidator::new();
    validator
        .observe(&GatewayEvent::Started(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("Started is valid");
    validator
        .observe(&GatewayEvent::ContentAdded(ContentItem::new(
            2,
            ContentKind::Reasoning,
        )))
        .expect("reasoning content is valid");

    let error = validator
        .observe(&GatewayEvent::TextDelta(TextDelta {
            content_index: 2,
            text: "not reasoning".to_owned(),
        }))
        .expect_err("delta kind must match the declared content");

    assert_eq!(error, EventSequenceError::InvalidDeltaTarget { index: 2 });
}

#[test]
fn validator_should_reject_reasoning_delta_for_text_content() {
    let mut validator = EventSequenceValidator::new();
    validator
        .observe(&GatewayEvent::Started(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("Started is valid");
    validator
        .observe(&GatewayEvent::ContentAdded(ContentItem::new(
            1,
            ContentKind::Text,
        )))
        .expect("text content is valid");

    let error = validator
        .observe(&GatewayEvent::ReasoningDelta(ReasoningDelta {
            content_index: 1,
            text: "not text".to_owned(),
        }))
        .expect_err("delta kind must match the declared content");

    assert_eq!(error, EventSequenceError::InvalidDeltaTarget { index: 1 });
}

#[test]
fn validator_should_reject_tool_delta_for_unknown_content() {
    let mut validator = EventSequenceValidator::new();
    validator
        .observe(&GatewayEvent::Started(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("Started is valid");

    let error = validator
        .observe(&GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: 4,
            call_id: "call_1".to_owned(),
            name: Some("lookup".to_owned()),
            arguments_delta: "{}".to_owned(),
        }))
        .expect_err("tool delta must reference declared tool content");

    assert_eq!(error, EventSequenceError::InvalidDeltaTarget { index: 4 });
}

#[test]
fn validator_should_reject_stream_without_completed() {
    let mut validator = EventSequenceValidator::new();
    validator
        .observe(&GatewayEvent::Started(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("Started is valid");

    assert_eq!(
        validator.finish(),
        Err(EventSequenceError::MissingCompleted)
    );
}

#[test]
fn validator_should_reject_event_after_completed() {
    let mut validator = EventSequenceValidator::new();
    validator
        .observe(&GatewayEvent::Started(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("Started is valid");
    validator
        .observe(&GatewayEvent::Completed(ResponseMeta::new(
            "resp_1",
            "smart-code",
        )))
        .expect("Completed is valid");

    let error = validator
        .observe(&GatewayEvent::ContentAdded(ContentItem::new(
            0,
            ContentKind::Text,
        )))
        .expect_err("Completed must be terminal");

    assert_eq!(error, EventSequenceError::EventAfterCompleted);
}

#[test]
fn validator_should_accept_reasoning_and_tool_call_stream() {
    let mut validator = EventSequenceValidator::new();
    let events = [
        GatewayEvent::Started(ResponseMeta::new("resp_1", "smart-code")),
        GatewayEvent::ContentAdded(ContentItem::new(0, ContentKind::Reasoning)),
        GatewayEvent::ReasoningDelta(ReasoningDelta {
            content_index: 0,
            text: "thinking".to_owned(),
        }),
        GatewayEvent::ContentAdded(ContentItem::new(1, ContentKind::ToolCall)),
        GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: 1,
            call_id: "call_1".to_owned(),
            name: Some("lookup".to_owned()),
            arguments_delta: "{\"q\":\"rust\"}".to_owned(),
        }),
        GatewayEvent::Completed(ResponseMeta::new("resp_1", "smart-code")),
    ];

    for event in &events {
        validator.observe(event).expect("sequence is canonical");
    }

    assert_eq!(validator.finish(), Ok(()));
}

#[test]
fn response_observation_snapshots_preserve_values_without_copying_large_payloads() {
    let metadata = format!("{{\"raw\":\"{}\"}}", "a".repeat(32 * 1024 - 10));
    let observation =
        ProviderResponseObservation::new(UpstreamTransport::new("http_sse").expect("transport"))
            .with_provider_metadata(
                ProviderResponseMetadata::new(metadata.clone()).expect("metadata"),
            )
            .with_client_headers(
                (0..32)
                    .map(|_| {
                        ProviderResponseHeader::new("set-cookie", Bytes::from_static(b"raw-\xff"))
                    })
                    .collect(),
            )
            .with_timings(ProviderResponseTimings {
                first_event_ms: Some(17),
                ..ProviderResponseTimings::default()
            });

    let allocations = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            std::hint::black_box(observation.clone());
        }
    });
    // 允许小标量字段的复制，但流式快照不能随大头集合和 metadata 重复分配
    assert!(allocations.bytes_total < 256 * 1_000, "{allocations:?}");
    assert_eq!(allocations.bytes_current, 0);

    let updated = observation
        .clone()
        .with_client_headers(Vec::new())
        .with_provider_metadata(
            ProviderResponseMetadata::new("{\"updated\":true}".to_owned()).expect("metadata"),
        )
        .with_timings(ProviderResponseTimings {
            first_event_ms: Some(23),
            ..ProviderResponseTimings::default()
        });
    assert_eq!(
        observation.provider_metadata().expect("metadata").as_json(),
        metadata
    );
    assert_eq!(observation.client_headers().len(), 32);
    assert!(
        observation.client_headers().iter().all(|header| {
            header.name() == "set-cookie" && header.value().as_ref() == b"raw-\xff"
        })
    );
    assert_eq!(observation.timings().first_event_ms, Some(17));
    assert_eq!(
        updated.provider_metadata().expect("metadata").as_json(),
        "{\"updated\":true}"
    );
    assert!(updated.client_headers().is_empty());
    assert_eq!(updated.timings().first_event_ms, Some(23));
}
