use gateway_core::event::{
    ContentItem, ContentKind, EventSequenceError, EventSequenceValidator, GatewayEvent,
    ReasoningDelta, ResponseMeta, TextDelta, ToolCallDelta,
};

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
