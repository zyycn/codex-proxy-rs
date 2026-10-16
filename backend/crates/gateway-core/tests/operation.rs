//! 验证生成请求保留不透明正文，并按协议解析能力与转换边界

use bytes::Bytes;
use gateway_core::operation::{
    Feature, GenerateRequest, ImageRequest, ImageRequestKind, Operation, OperationKind,
    ProtocolPayload, ProviderHttpHeader, ProviderHttpMethod, ProviderHttpRequest,
    ProviderSessionState, RawHttpPayload, RawJsonPayload, StandaloneSearchRequest,
    TokenCountRequest,
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
fn middleware_protocol_change_keeps_source_capability_facts_without_interpreting_target_json() {
    let operation = Operation::Generate(generate(json!({
        "model":"gpt-test",
        "input":[{"type":"message","role":"user","content":[{
            "type":"input_image","image_url":"https://example.invalid/image.png"
        }]}],
        "tools":[{"type":"function","name":"lookup"}],
        "reasoning":{"effort":"future-level"},
        "text":{"format":{"type":"json_schema","name":"result","schema":{}}},
        "previous_response_id":"resp_previous",
        "max_output_tokens":1024
    })));
    let source_requirements = operation.capability_requirements();
    let translated = operation
        .replace_middleware_wire(
            "openai_chat_completions",
            Bytes::from_static(
                br#"{"model":"gpt-test","messages":[{"role":"user","content":"hello"}],"max_completion_tokens":1024}"#,
            ),
        )
        .expect("direct translation");

    assert_eq!(translated.protocol(), "openai_chat_completions");
    assert_eq!(translated.capability_requirements(), source_requirements);
    let replaced = translated
        .replace_protocol_wire(
            Some(Bytes::from_static(
                br#"{"model":"gpt-test","messages":[{"role":"user","content":"enriched"}]}"#,
            )),
            Map::new(),
        )
        .expect("post-translation processing");
    assert_eq!(replaced.capability_requirements(), source_requirements);
}

#[test]
fn middleware_processing_recomputes_requirements_but_protocol_change_is_direct_once() {
    let operation = Operation::Generate(generate(json!({
        "model":"gpt-test",
        "input":"hello"
    })));
    let enriched = operation
        .replace_middleware_wire(
            "openai",
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello","max_output_tokens":64}"#),
        )
        .expect("same-protocol middleware processing");
    assert_eq!(
        enriched.capability_requirements().requested_output_tokens(),
        Some(64)
    );
    let translated = enriched
        .replace_middleware_wire(
            "openai_chat_completions",
            Bytes::from_static(
                br#"{"model":"gpt-test","messages":[{"role":"user","content":"hello"}],"max_completion_tokens":64}"#,
            ),
        )
        .expect("first direct translation");
    assert!(
        translated
            .replace_middleware_wire(
                "anthropic_messages",
                Bytes::from_static(br#"{"model":"gpt-test","messages":[]}"#),
            )
            .is_err()
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
fn image_generation_detection_distinguishes_tools_from_image_inputs_and_passive_namespaces() {
    for body in [
        json!({"tools":[{"type":"image_generation"}]}),
        json!({"tool_choice":{"type":"image_generation"}}),
        json!({"tool_choice":{"type":"allowed_tools","tools":[{"type":"image_generation"}]}}),
        json!({"input":[{"type":"additional_tools","tools":[{"type":"image_generation"}]}]}),
    ] {
        assert!(
            generate(body.clone()).image_generation_requested(),
            "{body}"
        );
    }
    for body in [
        json!({"input":"Please generate an image"}),
        json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,AA=="}]}]}),
        json!({"tools":[{"type":"namespace","name":"image_gen","tools":[{"type":"function","name":"imagegen"}]}]}),
        json!({"tools":[{"type":"function","name":"image_generation"}]}),
    ] {
        assert!(
            !generate(body.clone()).image_generation_requested(),
            "{body}"
        );
    }
    let private = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "private",
            json!({"tools":[{"type":"image_generation"}]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    );
    assert!(!private.image_generation_requested());
}

#[test]
fn image_generation_fact_survives_cross_protocol_translation() {
    let operation = Operation::Generate(generate(json!({"tools":[{"type":"image_generation"}]})));
    let translated = operation
        .replace_middleware_wire(
            "openai_chat_completions",
            Bytes::from_static(br#"{"model":"gpt-test","messages":[]}"#),
        )
        .unwrap();
    assert!(translated.image_generation_requested());
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
    let search = Operation::Search(StandaloneSearchRequest::from_raw_json(
        RawJsonPayload::new(
            "openai",
            Bytes::from_static(br#"{ "model":"gpt-test", "commands":{} }"#),
        )
        .expect("OpenAI search payload"),
    ));
    let count = Operation::CountTokens(TokenCountRequest::from_raw_json(
        RawJsonPayload::new("token-count", Bytes::from_static(br#"{"input":"hello"}"#))
            .expect("token count payload"),
    ));
    let provider_http = Operation::ProviderHttp(
        ProviderHttpRequest::new(
            "inspect",
            ProviderHttpMethod::Post,
            Some("mode=future".to_owned()),
            vec![ProviderHttpHeader::new(
                "x-opaque",
                Bytes::from_static(b"private-header"),
            )],
            RawHttpPayload::new("provider-http", Bytes::from_static(b"private-body"))
                .expect("provider HTTP payload"),
        )
        .expect("provider HTTP request"),
    );

    assert_eq!(generate.kind(), OperationKind::Generate);
    assert_eq!(image.kind(), OperationKind::GenerateImage);
    assert_eq!(search.kind(), OperationKind::Search);
    assert_eq!(count.kind(), OperationKind::CountTokens);
    assert_eq!(provider_http.kind(), OperationKind::ProviderHttp);
    assert_eq!(OperationKind::Search.as_str(), "search");
    assert_eq!(OperationKind::CountTokens.as_str(), "count_tokens");
    assert_eq!(OperationKind::ProviderHttp.as_str(), "provider_http");
    assert!(image.image_generation_requested());
    assert!(!search.image_generation_requested());
    assert!(!format!("{provider_http:?}").contains("private-header"));
    assert!(!format!("{provider_http:?}").contains("private-body"));
}

#[test]
fn provider_http_should_reject_urls_controls_and_unbounded_headers() {
    let payload =
        || RawHttpPayload::new("provider-http", Bytes::new()).expect("provider HTTP payload");
    assert!(
        ProviderHttpRequest::new(
            "https://example.invalid/models",
            ProviderHttpMethod::Get,
            None,
            Vec::new(),
            payload(),
        )
        .is_err()
    );
    assert!(
        ProviderHttpRequest::new(
            "models",
            ProviderHttpMethod::Get,
            Some("ok=1\r\ninjected=1".to_owned()),
            Vec::new(),
            payload(),
        )
        .is_err()
    );
    assert!(
        ProviderHttpRequest::new(
            "models",
            ProviderHttpMethod::Get,
            None,
            vec![ProviderHttpHeader::new(
                "x-large",
                Bytes::from(vec![b'x'; 8 * 1024 + 1]),
            )],
            payload(),
        )
        .is_err()
    );
}

#[test]
fn standalone_search_should_keep_raw_body_and_context_opaque() {
    let body = Bytes::from_static(
        br#"{ "model":"gpt-future", "commands":{}, "future":9007199254740993, "future":2 }"#,
    );
    let secret = "private-turn-metadata";
    let payload = RawJsonPayload::new("openai", body.clone())
        .expect("OpenAI search payload")
        .with_context(Map::from_iter([(
            "turn_metadata".to_owned(),
            Value::String(secret.to_owned()),
        )]));
    let request = StandaloneSearchRequest::from_raw_json(payload);

    assert_eq!(request.payload().body(), &body);
    assert_eq!(
        request.payload().context().get("turn_metadata"),
        Some(&Value::String(secret.to_owned()))
    );
    assert!(!format!("{request:?}").contains(secret));
    assert!(!format!("{request:?}").contains("gpt-future"));
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
