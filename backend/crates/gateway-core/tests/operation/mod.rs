use bytes::Bytes;
use gateway_core::operation::{
    Feature, GenerateRequest, ImageRequest, ImageRequestKind, Operation, OperationKind,
    ProtocolPayload, ProviderSessionState, RawJsonPayload,
};
use serde_json::{Map, Value, json};

fn generate(body: Value) -> GenerateRequest {
    let body = body.as_object().expect("request object").clone();
    GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    )
}

fn image(kind: ImageRequestKind, body: Value) -> ImageRequest {
    ImageRequest::from_raw_json(
        kind,
        RawJsonPayload::new(
            "openai",
            Bytes::from(serde_json::to_vec(&body).expect("image JSON")),
        )
        .expect("OpenAI payload"),
    )
}

#[test]
fn generate_request_should_keep_client_body_opaque_and_redacted() {
    let secret = "private prompt body";
    let body = json!({
        "model": "gpt-test",
        "input": [{"type": "message", "role": "user", "content": secret}],
        "future_provider_field": {"keep": true},
    });
    let request = generate(body.clone());

    assert_eq!(
        request.protocol_payload().body(),
        body.as_object().expect("request object")
    );
    assert!(!format!("{request:?}").contains(secret));
}

#[test]
fn capability_requirements_should_read_known_openai_fields_without_rewriting_body() {
    let request = generate(json!({
        "model": "gpt-test",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_image", "image_url": "https://example.invalid/image.png"}],
        }],
        "tools": [{"type": "function", "name": "lookup"}],
        "reasoning": {"effort": "future-level"},
        "text": {"format": {"type": "json_schema", "name": "result", "schema": {}}},
        "previous_response_id": "resp_previous",
        "max_output_tokens": 1024,
    }));
    let requirements = Operation::Generate(request).capability_requirements();

    assert_eq!(requirements.requested_output_tokens(), Some(1024));
    assert!(requirements.features().contains(&Feature::Tools));
    assert!(requirements.features().contains(&Feature::Vision));
    assert!(requirements.features().contains(&Feature::Reasoning));
    assert!(requirements.features().contains(&Feature::JsonSchema));
    assert!(
        requirements
            .features()
            .contains(&Feature::NativeContinuation)
    );
}

#[test]
fn prompt_cache_and_image_generation_should_be_read_from_raw_body() {
    let request = generate(json!({
        "model": "gpt-test",
        "prompt_cache_key": "private-cache-route",
        "tools": [{"type": "image_generation"}],
    }));

    assert_eq!(request.prompt_cache_key(), Some("private-cache-route"));
    assert!(request.image_generation_requested());
    assert!(!format!("{request:?}").contains("private-cache-route"));
}

#[test]
fn protocol_payload_context_should_be_opaque_and_separate_from_wire_body() {
    let secret = "connection-only-value";
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::from("gpt-test"));
    let mut context = Map::new();
    context.insert("turn_state".to_owned(), Value::from(secret));
    let payload = ProtocolPayload::json_object("openai", body)
        .expect("protocol payload is valid")
        .with_context(context);

    assert_eq!(
        payload.context().get("turn_state"),
        Some(&Value::from(secret))
    );
    assert!(!format!("{payload:?}").contains(secret));
}

#[test]
fn provider_session_state_should_only_be_visible_to_its_provider() {
    let request = generate(json!({"model": "gpt-test"})).with_provider_session_state(
        ProviderSessionState::new(
            "xai",
            Map::from_iter([("session_id".to_owned(), json!("s"))]),
        )
        .expect("xAI state"),
    );

    assert!(request.provider_session_state("xai").is_some());
    assert!(request.provider_session_state("openai").is_none());
}

#[test]
fn operation_kind_should_remain_stable() {
    let generate = Operation::Generate(generate(json!({"model": "gpt-test"})));
    let image = Operation::GenerateImage(image(
        ImageRequestKind::Generation,
        json!({"model": "gpt-image-2", "prompt": "draw"}),
    ));

    assert_eq!(generate.kind(), OperationKind::Generate);
    assert_eq!(image.kind(), OperationKind::GenerateImage);
    assert!(image.image_generation_requested());
}

#[test]
fn image_request_should_preserve_generation_and_edit_payloads_opaque() {
    let generation_body = json!({
        "model": "gpt-image-2",
        "prompt": "draw",
        "future_official_field": {"keep": true},
    });
    let edit_body = json!({
        "model": "gpt-image-2",
        "images": [{"image_url": "data:image/png;base64,AAAA"}],
        "prompt": "edit",
        "future_official_field": [1, 2, 3],
    });
    let generation = image(ImageRequestKind::Generation, generation_body.clone());
    let edit = image(ImageRequestKind::Edit, edit_body.clone());

    assert_eq!(generation.kind(), ImageRequestKind::Generation);
    assert_eq!(edit.kind(), ImageRequestKind::Edit);
    assert_eq!(
        serde_json::from_slice::<Value>(generation.payload().body()).expect("generation JSON"),
        generation_body
    );
    assert_eq!(
        serde_json::from_slice::<Value>(edit.payload().body()).expect("edit JSON"),
        edit_body
    );
    assert!(!format!("{edit:?}").contains("data:image/png"));
}
