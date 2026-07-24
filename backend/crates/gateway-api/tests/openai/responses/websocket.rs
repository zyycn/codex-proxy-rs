use gateway_api::openai::responses::ResponseCreateFrameError;
use gateway_core::operation::Operation;
use serde_json::json;

use super::decode_response_create;

#[test]
fn response_create_should_default_to_the_websocket_streaming_contract() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "store": false
        })
        .to_string(),
    )
    .expect("decode response.create");

    assert!(decoded.metadata().stream());
    assert!(!decoded.metadata().store());
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };
    assert!(
        request
            .protocol_payload()
            .and_then(|payload| payload.body().get("stream"))
            .is_none()
    );
}

#[test]
fn response_create_should_preserve_provider_options_as_opaque_wire_body() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "stream": true,
            "provider_options": {
                "version": "v1",
                "providers": {
                    "openai": {"schema_version": 1, "transport": "websocket"}
                }
            }
        })
        .to_string(),
    )
    .expect("decode opaque provider options");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };
    let payload = request.protocol_payload().expect("OpenAI wire payload");

    assert_eq!(
        payload.body().get("provider_options"),
        Some(&json!({
            "version": "v1",
            "providers": {
                "openai": {"schema_version": 1, "transport": "websocket"}
            }
        }))
    );
    assert!(payload.context().get("provider_options").is_none());
}

#[test]
fn response_create_should_preserve_compaction_trigger_for_openai() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": [
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ]
        })
        .to_string(),
    )
    .expect("decode OpenAI response.create");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("OpenAI response.create must remain Generate");
    };

    assert_eq!(
        request
            .protocol_payload()
            .and_then(|payload| payload.body().get("input"))
            .and_then(|input| input.pointer("/1/type")),
        Some(&json!("compaction_trigger"))
    );
}

#[test]
fn response_create_should_reject_explicit_non_streaming_requests() {
    let error = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "stream": false
        })
        .to_string(),
    )
    .expect_err("WebSocket requests must stream");

    assert_eq!(error, ResponseCreateFrameError::StreamingRequired);
}

#[test]
fn response_create_should_reject_invalid_frame_shapes() {
    for (payload, expected) in [
        ("not-json", ResponseCreateFrameError::InvalidJson),
        ("[]", ResponseCreateFrameError::ExpectedObject),
        (
            r#"{"type":"future.message","model":"smart-code","input":"hello"}"#,
            ResponseCreateFrameError::UnsupportedType,
        ),
    ] {
        assert_eq!(
            decode_response_create(payload).expect_err("invalid frame"),
            expected
        );
    }
}

#[test]
fn response_create_should_reject_non_boolean_stream_without_disclosing_body_values() {
    let prompt = "private-websocket-prompt-marker";
    let opaque_stream_value = "private-websocket-option-marker";
    let error = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": prompt,
            "stream": opaque_stream_value
        })
        .to_string(),
    )
    .expect_err("WebSocket response.create must explicitly enable streaming");
    let rendered = format!("{error:?}\n{error}");

    assert_eq!(error, ResponseCreateFrameError::StreamingRequired);
    assert!(!rendered.contains(prompt));
    assert!(!rendered.contains(opaque_stream_value));
}
