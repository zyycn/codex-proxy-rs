use gateway_api::openai::responses::{ResponseCreateFrameError, decode_response_create};
use gateway_core::operation::Operation;
use serde_json::json;

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
    assert!(matches!(decoded.operation(), Operation::Generate(_)));
}

#[test]
fn response_create_should_preserve_current_provider_options() {
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
    .expect("decode provider options");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };

    assert_eq!(
        request
            .provider_options()
            .get("openai")
            .and_then(|options| options.get("transport")),
        Some(&json!("websocket"))
    );
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
fn response_create_errors_should_not_retain_prompt_or_unknown_field_values() {
    let prompt = "private-websocket-prompt-marker";
    let secret = "private-websocket-option-marker";
    let error = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": prompt,
            "metadata": {"secret": secret},
            "stream": secret
        })
        .to_string(),
    )
    .expect_err("invalid stream field");
    let rendered = format!("{error:?}\n{error}");

    assert!(!rendered.contains(prompt));
    assert!(!rendered.contains(secret));
}
