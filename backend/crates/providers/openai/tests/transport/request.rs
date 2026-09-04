use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use serde_json::{Map, Value, json};

use provider_openai::encode_generate_request;

fn request(body: Map<String, Value>) -> GenerateRequest {
    GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    )
}

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
    let request = request(body);

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
        request(Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), Value::String(input.to_owned())),
        ]))
    };

    for input in ["private stable prompt", "different private prompt"] {
        let encoded =
            encode_generate_request(&request(input), "gpt-routed").expect("encoded request");
        assert!(encoded.local_conversation_id.is_none());
    }
}

#[test]
fn encoder_should_patch_model_and_preserve_supported_generate_semantics() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("secret prompt")),
        (
            "tools".to_owned(),
            json!([{"type": "function", "name": "lookup", "strict": true}]),
        ),
        ("store".to_owned(), json!(false)),
        (
            "reasoning".to_owned(),
            json!({"effort": "high", "summary": "concise"}),
        ),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");
    assert_eq!(encoded.body().get("model"), Some(&json!("gpt-test")));
    assert!(encoded.body().get("stream").is_none());
    assert_eq!(encoded.body().get("store"), Some(&json!(false)));
    let body = Value::Object(encoded.body().clone());
    assert_eq!(body.pointer("/tools/0/strict"), Some(&json!(true)));
    assert_eq!(body.pointer("/reasoning/effort"), Some(&json!("high")));
    assert!(!encoded.force_http_sse);
}

#[test]
fn encoder_should_remove_unsupported_fields_from_upstream_body() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("hello")),
        ("max_output_tokens".to_owned(), json!(512)),
        ("max_tokens".to_owned(), json!(256)),
        ("temperature".to_owned(), json!(0.2)),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(
        Value::Object(encoded.body().clone()),
        json!({
            "model": "gpt-test",
            "input": "hello",
            "max_tokens": 256,
        })
    );
}

#[test]
fn encoder_should_preserve_client_store_intent() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("persist inside gateway")),
        ("store".to_owned(), json!(true)),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(encoded.body().get("store"), Some(&json!(true)));
}

#[test]
fn encoder_should_forward_the_client_prompt_cache_key() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("cache prefix")),
        ("prompt_cache_key".to_owned(), json!("cache-route")),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(
        encoded.body().get("prompt_cache_key"),
        Some(&json!("cache-route"))
    );
}

#[test]
fn encoder_should_restore_conversation_fallback_after_body_and_context_values() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("prompt")),
        ("prompt_cache_key".to_owned(), json!("cache-conversation")),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(
        encoded.client_conversation_id.as_deref(),
        Some("cache-conversation")
    );
}

#[test]
fn encoder_should_ignore_legacy_context_aliases_and_metadata_fallbacks() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("prompt")),
        ("turn_state".to_owned(), json!("legacy-turn-state")),
        (
            "x-codex-turn-state".to_owned(),
            json!("legacy-header-turn-state"),
        ),
        ("turn_metadata".to_owned(), json!("legacy-turn-metadata")),
        ("beta_features".to_owned(), json!("legacy-beta")),
        ("include_timing_metrics".to_owned(), json!("legacy-timing")),
        ("codex_window_id".to_owned(), json!("legacy-window")),
        (
            "x-codex-window-id".to_owned(),
            json!("legacy-header-window"),
        ),
        ("parent_thread_id".to_owned(), json!("legacy-parent")),
        (
            "x-codex-parent-thread-id".to_owned(),
            json!("legacy-header-parent"),
        ),
        ("conversationId".to_owned(), json!("legacy-conversation")),
        ("sessionId".to_owned(), json!("legacy-session")),
        ("threadId".to_owned(), json!("legacy-thread")),
        ("client_request_id".to_owned(), json!("legacy-request")),
        ("clientRequestId".to_owned(), json!("legacy-camel-request")),
        ("turnId".to_owned(), json!("legacy-turn")),
        ("x-codex-turn-id".to_owned(), json!("legacy-header-turn")),
        (
            "client_metadata".to_owned(),
            json!({
                "turnState": "metadata-turn-state",
                "turnMetadata": "metadata-turn-metadata",
                "conversation_id": "metadata-conversation",
                "session_id": "metadata-session",
                "thread_id": "metadata-thread",
                "x-codex-window-id": "metadata-window"
            }),
        ),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(
        (
            encoded.turn_state,
            encoded.turn_metadata,
            encoded.beta_features,
            encoded.include_timing_metrics,
            encoded.codex_window_id,
            encoded.parent_thread_id,
            encoded.client_conversation_id,
            encoded.client_session_id,
            encoded.client_thread_id,
            encoded.client_request_id,
            encoded.client_turn_id,
        ),
        (
            None, None, None, None, None, None, None, None, None, None, None,
        )
    );
}

#[test]
fn header_context_should_win_over_body_topline_aliases() {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
            ("turnState".to_owned(), json!("body-turn-state")),
            ("turnMetadata".to_owned(), json!("body-turn-metadata")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([
        (
            "turn_state".to_owned(),
            Value::String("header-turn-state".to_owned()),
        ),
        (
            "turn_metadata".to_owned(),
            Value::String("header-turn-metadata".to_owned()),
        ),
    ]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(encoded.turn_state.as_deref(), Some("header-turn-state"));
    assert_eq!(
        encoded.turn_metadata.as_deref(),
        Some("header-turn-metadata")
    );
}

#[test]
fn encoder_should_preserve_downstream_websocket_connection_identity_outside_wire_body() {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "downstream_websocket_connection_id".to_owned(),
        Value::String("ws_downstream_a".to_owned()),
    )]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(
        encoded.downstream_websocket_connection_id.as_deref(),
        Some("ws_downstream_a")
    );
    assert!(
        !encoded
            .body()
            .contains_key("downstream_websocket_connection_id")
    );
}

#[test]
fn body_topline_alias_should_only_fill_an_absent_header_context() {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
            ("turnState".to_owned(), json!("body-turn-state")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "turn_metadata".to_owned(),
        Value::String("header-turn-metadata".to_owned()),
    )]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(encoded.turn_state.as_deref(), Some("body-turn-state"));
    assert_eq!(
        encoded.turn_metadata.as_deref(),
        Some("header-turn-metadata")
    );
}

#[test]
fn encoder_should_project_explicit_websocket_transport_without_touching_body() {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "use_websocket".to_owned(),
        Value::Bool(true),
    )]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert!(encoded.use_websocket);
    assert!(!encoded.force_http_sse);
    assert!(encoded.body().get("transport").is_none());
}

#[test]
fn encoder_should_project_explicit_http_transport_without_touching_body() {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "use_websocket".to_owned(),
        Value::Bool(false),
    )]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert!(!encoded.use_websocket);
    assert!(encoded.force_http_sse);
    assert!(encoded.body().get("transport").is_none());
}

#[test]
fn encoder_should_preserve_opaque_provider_options_without_interpreting_them() {
    let opaque_options = json!({
        "version": "future-version",
        "providers": {
            "openai": {"secret_future_option": "must-survive"}
        }
    });
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
            ("provider_options".to_owned(), opaque_options.clone()),
        ]),
    )
    .expect("OpenAI payload");
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test").expect("opaque options encode");

    assert_eq!(
        encoded.body().get("provider_options"),
        Some(&opaque_options)
    );
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
    let payload = payload.with_context(Map::from_iter([
        (
            "responses_lite".to_owned(),
            Value::String("true".to_owned()),
        ),
        (
            "memgen_request".to_owned(),
            Value::String("true".to_owned()),
        ),
    ]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test").expect("encode");

    assert_eq!(encoded.responses_lite.as_deref(), Some("true"));
    assert_eq!(encoded.memgen_request.as_deref(), Some("true"));
    assert!(encoded.body().get("responses_lite").is_none());
    assert!(encoded.body().get("memgen_request").is_none());
}

#[test]
fn observability_semantics_should_reuse_codex_turn_metadata() {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "turn_metadata".to_owned(),
        Value::String(r#"{"request_kind":"compaction","subagent_kind":"review"}"#.to_owned()),
    )]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let semantics = encode_generate_request(&request, "observability")
        .expect("observability request should encode")
        .semantics();

    assert_eq!(semantics.request_kind.as_deref(), Some("compaction"));
    assert_eq!(semantics.subagent_kind.as_deref(), Some("review"));
    assert!(semantics.compact);
}

#[test]
fn encoder_should_extract_subagent_kind_from_wire_or_turn_metadata() {
    let metadata_request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("prompt")),
        (
            "client_metadata".to_owned(),
            json!({"x-openai-subagent": "review"}),
        ),
    ]));
    let metadata_encoded =
        encode_generate_request(&metadata_request, "gpt-test").expect("encode metadata request");
    assert_eq!(metadata_encoded.subagent_kind().as_deref(), Some("review"));

    let turn_metadata_payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("prompt")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "turn_metadata".to_owned(),
        Value::String(r#"{"subagent_kind":"worker"}"#.to_owned()),
    )]));
    let turn_metadata_request = GenerateRequest::from_protocol_payload(turn_metadata_payload);
    let turn_metadata_encoded = encode_generate_request(&turn_metadata_request, "gpt-test")
        .expect("encode turn metadata request");
    assert_eq!(
        turn_metadata_encoded.subagent_kind().as_deref(),
        Some("worker")
    );
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
    let request = GenerateRequest::from_protocol_payload(payload);

    let semantics = encode_generate_request(&request, "observability")
        .expect("transparent OpenAI request should encode")
        .semantics();

    assert_eq!(semantics.reasoning_preset, Some("ultra"));
    assert!(semantics.compact);
}
