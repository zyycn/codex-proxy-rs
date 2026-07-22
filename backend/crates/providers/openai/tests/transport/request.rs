use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, ProtocolPayload, ProviderOptions,
    ReasoningEffort, ReasoningRequirement, ReasoningSummary, ResponsePersistence, ToolDefinition,
};
use serde_json::{Map, Value, json};

use provider_openai::{CodexRequestEncodeError, codex_request_semantics, encode_generate_request};

#[test]
fn encoder_should_preserve_openai_wire_fields_without_deriving_accountless_pool_identity() {
    let body = Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        (
            "input".to_owned(),
            json!([
                {"role":"user","content":"private stable prompt"},
                {"type":"compaction_trigger"}
            ]),
        ),
        ("include".to_owned(), json!(["reasoning.encrypted_content"])),
        ("tool_choice".to_owned(), json!("auto")),
        ("service_tier".to_owned(), json!("priority")),
        ("conversation_id".to_owned(), json!("private-conversation")),
        ("session_id".to_owned(), json!("private-session")),
        ("turnState".to_owned(), json!("private-turn-state")),
        ("future_official_field".to_owned(), json!({"enabled": true})),
    ]);
    let payload = ProtocolPayload::json_object("openai", body).expect("OpenAI payload");
    let request = GenerateRequest::from_protocol_payload(Vec::new(), payload);

    let encoded = encode_generate_request(&request, "gpt-routed").expect("encode wire payload");

    assert_eq!(encoded.body().get("model"), Some(&json!("gpt-routed")));
    assert!(encoded.body().get("stream").is_none());
    assert!(encoded.body().get("store").is_none());
    assert_eq!(encoded.body().get("tool_choice"), Some(&json!("auto")));
    assert_eq!(
        encoded.body().get("future_official_field"),
        Some(&json!({"enabled": true}))
    );
    assert_eq!(encoded.turn_state.as_deref(), Some("private-turn-state"));
    assert_eq!(
        encoded.client_session_id.as_deref(),
        Some("private-session")
    );
    assert!(encoded.local_conversation_id.is_none());
    assert!(!format!("{encoded:?}").contains("private stable prompt"));
    assert_eq!(
        Value::Object(encoded.body().clone()).pointer("/input/1/type"),
        Some(&json!("compaction_trigger"))
    );
}

#[test]
fn encoder_should_never_hash_prompt_content_into_an_accountless_pool_identity() {
    let request = |input: &str| {
        let payload = ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("client-model")),
                ("input".to_owned(), Value::String(input.to_owned())),
            ]),
        )
        .expect("OpenAI payload");
        GenerateRequest::from_protocol_payload(Vec::new(), payload)
    };

    for input in ["private stable prompt", "different private prompt"] {
        let encoded =
            encode_generate_request(&request(input), "gpt-routed").expect("encoded request");
        assert!(encoded.local_conversation_id.is_none());
    }
}

#[test]
fn encoder_should_project_typed_generate_semantics() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("secret prompt".to_owned())],
    )
    .expect("message");
    let tool = ToolDefinition::new(
        "lookup",
        Some("Lookup a value".to_owned()),
        json!({"type":"object"})
            .as_object()
            .cloned()
            .expect("object"),
    )
    .expect("tool")
    .with_strict(true);
    let request = GenerateRequest::new(vec![message])
        .expect("generate")
        .with_tools(vec![tool])
        .with_max_output_tokens(512)
        .with_response_persistence(ResponsePersistence::DoNotStore)
        .with_reasoning(ReasoningRequirement {
            effort: Some(ReasoningEffort::High),
            summary: Some(ReasoningSummary::Concise),
        });

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");
    assert_eq!(encoded.body().get("model"), Some(&json!("gpt-test")));
    assert_eq!(encoded.body().get("stream"), Some(&json!(true)));
    assert_eq!(encoded.body().get("max_output_tokens"), Some(&json!(512)));
    assert_eq!(encoded.body().get("store"), Some(&json!(false)));
    let body = Value::Object(encoded.body().clone());
    assert_eq!(body.pointer("/tools/0/strict"), Some(&json!(true)));
    assert_eq!(body.pointer("/reasoning/effort"), Some(&json!("high")));
    assert!(!encoded.force_http_sse);
}

#[test]
fn downstream_store_intent_should_not_enable_upstream_storage() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("persist inside gateway".to_owned())],
    )
    .expect("message");
    let request = GenerateRequest::new(vec![message])
        .expect("generate")
        .with_response_persistence(ResponsePersistence::Store);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(encoded.body().get("store"), Some(&json!(true)));
}

#[test]
fn encoder_should_forward_the_common_prompt_cache_key() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("cache prefix".to_owned())],
    )
    .expect("message");
    let request = GenerateRequest::new(vec![message])
        .expect("generate")
        .with_prompt_cache_key("cache-route");

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(
        encoded.body().get("prompt_cache_key"),
        Some(&json!("cache-route"))
    );
}

#[test]
fn encoder_should_project_explicit_websocket_transport_without_touching_body() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("prompt".to_owned())],
    )
    .expect("message");
    let mut providers = ProviderOptions::new();
    providers
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                ("transport".to_owned(), json!("websocket")),
            ]),
        )
        .expect("provider options");
    let request = GenerateRequest::new(vec![message])
        .expect("generate")
        .with_provider_options(providers);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert!(encoded.use_websocket);
    assert!(!encoded.force_http_sse);
    assert!(encoded.body().get("transport").is_none());
}

#[test]
fn encoder_should_project_explicit_http_transport_without_touching_body() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("prompt".to_owned())],
    )
    .expect("message");
    let mut providers = ProviderOptions::new();
    providers
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                ("transport".to_owned(), json!("http_sse")),
            ]),
        )
        .expect("provider options");
    let request = GenerateRequest::new(vec![message])
        .expect("generate")
        .with_provider_options(providers);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert!(!encoded.use_websocket);
    assert!(encoded.force_http_sse);
    assert!(encoded.body().get("transport").is_none());
}

#[test]
fn encoder_should_reject_unknown_codex_options_without_echoing_values() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("prompt".to_owned())],
    )
    .expect("message");
    let mut providers = ProviderOptions::new();
    providers
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                ("secret_future_option".to_owned(), json!("must-not-leak")),
            ]),
        )
        .expect("provider options");
    let request = GenerateRequest::new(vec![message])
        .expect("generate")
        .with_provider_options(providers);

    let error =
        encode_generate_request(&request, "gpt-test").expect_err("unknown option must fail closed");
    assert_eq!(error, CodexRequestEncodeError::UnsupportedProviderOption);
    assert!(!format!("{error:?}").contains("must-not-leak"));
}

#[test]
fn encoder_should_project_lite_and_memgen_options_to_transport_state_only() {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
            (
                "client_metadata".to_owned(),
                json!({
                    "ws_request_header_x_openai_internal_codex_responses_lite": "false",
                    "x-openai-memgen-request": "false"
                }),
            ),
        ]),
    )
    .expect("OpenAI payload");
    let mut providers = ProviderOptions::new();
    providers
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                ("responses_lite".to_owned(), json!("true")),
                ("memgen_request".to_owned(), json!("true")),
            ]),
        )
        .expect("provider options");
    let request = GenerateRequest::from_protocol_payload(Vec::new(), payload)
        .with_provider_options(providers);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(encoded.responses_lite.as_deref(), Some("true"));
    assert_eq!(encoded.memgen_request.as_deref(), Some("true"));
    assert!(encoded.body().get("responses_lite").is_none());
    assert!(encoded.body().get("memgen_request").is_none());
}

#[test]
fn observability_semantics_should_reuse_codex_turn_metadata() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("prompt".to_owned())],
    )
    .expect("message");
    let mut providers = ProviderOptions::new();
    providers
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                (
                    "turn_metadata".to_owned(),
                    json!(r#"{"request_kind":"compaction","subagent_kind":"review"}"#),
                ),
            ]),
        )
        .expect("provider options");
    let request = GenerateRequest::new(vec![message])
        .expect("generate")
        .with_provider_options(providers);

    let semantics = codex_request_semantics(&request);

    assert_eq!(semantics.request_kind.as_deref(), Some("compaction"));
    assert_eq!(semantics.subagent_kind.as_deref(), Some("review"));
    assert!(semantics.compact);
}

#[test]
fn observability_semantics_should_use_the_transparent_openai_payload() {
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({
            "model": "gpt-test",
            "reasoning": {"effort": "max"},
            "input": [
                {
                    "type": "message",
                    "role": "developer",
                    "content": [{
                        "type": "input_text",
                        "text": "<multi_agent_mode>Proactive multi-agent delegation is active.</multi_agent_mode>"
                    }]
                },
                {"type": "compaction_trigger"}
            ]
        })
        .as_object()
        .expect("request object")
        .clone(),
    )
    .expect("OpenAI payload");
    let request = GenerateRequest::from_protocol_payload(Vec::new(), payload);

    let semantics = codex_request_semantics(&request);

    assert_eq!(semantics.reasoning_preset, Some("ultra"));
    assert!(semantics.compact);
}
