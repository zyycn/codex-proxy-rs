use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};

use provider_xai::{GrokRequestEncodeError, GrokResponsesRequest};

fn raw_request(body: Value) -> GenerateRequest {
    let Value::Object(body) = body else {
        panic!("request fixture must be an object");
    };
    GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    )
}

fn client_key() -> ClientApiKeyId {
    ClientApiKeyId::new("key_xai_request_test").expect("client key id")
}

#[test]
fn encoder_should_preserve_raw_images_hosted_tools_and_unknown_fields() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": [{
            "role": "user",
            "content": [
                {"type": "input_text", "text": "describe"},
                {
                    "type": "input_image",
                    "image_url": "data:image/png;base64,AQID",
                    "detail": "original",
                    "future_image_field": {"keep": true}
                }
            ]
        }],
        "tools": [
            {"type": "web_search_preview", "search_context_size": "high"},
            {"type": "code_interpreter", "container": {"type": "auto"}},
            {"type": "x_search", "future": [1, 2, 3]}
        ],
        "tool_choice": "auto",
        "future_official_field": {"nested": [true, 7]},
        "stream": false,
        "store": true
    }));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-routed", &client_key()).expect("raw request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/model"), Some(&json!("grok-routed")));
    assert_eq!(body.pointer("/stream"), Some(&json!(true)));
    assert_eq!(body.pointer("/store"), Some(&json!(true)));
    assert_eq!(
        body.pointer("/include"),
        Some(&json!(["reasoning.encrypted_content"]))
    );
    assert_eq!(
        body.pointer("/input/0/content/1/detail"),
        Some(&json!("high"))
    );
    assert_eq!(body.pointer("/input/0/content/1/future_image_field"), None);
    assert_eq!(
        body.pointer("/tools/0"),
        Some(&json!({"type": "web_search"}))
    );
    assert_eq!(
        body.pointer("/tools/2"),
        Some(&json!({"type": "x_search", "future": [1, 2, 3]}))
    );
    assert_eq!(
        body.pointer("/future_official_field"),
        Some(&json!({"nested": [true, 7]}))
    );
}

#[test]
fn encoder_should_strip_all_client_metadata_before_grok_build() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "client_metadata": {
            "x-openai-subagent": "review",
            "application_tag": "preserve-me"
        },
        "metadata": {"application_tag": "keep-this"}
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
        .expect("sanitized request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/client_metadata"), None);
    assert_eq!(
        body.pointer("/metadata/application_tag"),
        Some(&json!("keep-this"))
    );

    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "client_metadata": "opaque-local-envelope"
    }));
    let encoded = GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
        .expect("scalar client metadata");
    assert_eq!(encoded.body().get("client_metadata"), None);
}

#[test]
fn encoder_should_apply_build_defaults_and_normalize_reasoning_effort() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "include": ["web_search_call.action.sources"],
        "reasoning": {"effort": "XHIGH", "summary": "auto"}
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("normalized request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/store"), Some(&json!(false)));
    assert_eq!(
        body.pointer("/include"),
        Some(&json!([
            "web_search_call.action.sources",
            "reasoning.encrypted_content"
        ]))
    );
    assert_eq!(body.pointer("/reasoning/effort"), Some(&json!("high")));
    assert_eq!(body.pointer("/reasoning/summary"), Some(&json!("auto")));

    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "reasoning": {"effort": "max"},
        "reasoning_effort": "extra_high",
        "reasoningEffort": "minimal"
    }));
    let encoded = GrokResponsesRequest::encode(&request, "xai/grok-4.6-latest", &client_key())
        .expect("aliased request");
    let body = Value::Object(encoded.body().clone());
    assert_eq!(body.pointer("/model"), Some(&json!("grok-4.6")));
    assert_eq!(body.pointer("/reasoning/effort"), Some(&json!("high")));
    assert_eq!(body.pointer("/reasoning_effort"), Some(&json!("high")));
    assert_eq!(body.pointer("/reasoningEffort"), None);

    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "reasoning": {"effort": "high", "summary": "auto"}
    }));
    let encoded = GrokResponsesRequest::encode(&request, "grok-composer-2.5-fast", &client_key())
        .expect("Composer request");
    let body = Value::Object(encoded.body().clone());
    assert_eq!(body.pointer("/reasoning"), None);

    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "store": null,
        "include": null,
        "reasoning": {"effort": "max"}
    }));
    let encoded = GrokResponsesRequest::encode(&request, "grok-composer-2.5-fast", &client_key())
        .expect("empty Composer reasoning");
    let body = Value::Object(encoded.body().clone());
    assert_eq!(body.pointer("/store"), Some(&json!(false)));
    assert_eq!(
        body.pointer("/include"),
        Some(&json!(["reasoning.encrypted_content"]))
    );
    assert_eq!(body.pointer("/reasoning"), None);

    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "reasoning": {}
    }));
    let encoded = GrokResponsesRequest::encode(&request, "grok-composer-2.5-fast", &client_key())
        .expect("Composer reasoning without effort");
    assert_eq!(
        Value::Object(encoded.body().clone()).pointer("/reasoning"),
        None
    );

    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "reasoning": {"effort": "max"}
    }));
    let encoded = GrokResponsesRequest::encode(&request, "tenant/foo/grok-4.6", &client_key())
        .expect("unknown provider prefix");
    assert_eq!(
        Value::Object(encoded.body().clone()).pointer("/reasoning/effort"),
        None
    );
}

#[test]
fn encoder_should_strip_grok_unsupported_fields() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "prompt_cache_retention": "24h",
        "safety_identifier": "user-1",
        "presence_penalty": 0.1,
        "presencePenalty": 0.2,
        "frequency_penalty": 0.3,
        "frequencyPenalty": 0.4,
        "stop": ["done"],
        "external_web_access": true,
        "metadata": {"external_web_access": false},
        "tools": [{
            "type": "function",
            "name": "lookup",
            "external_web_access": true,
            "parameters": {
                "type": "object",
                "properties": {"q": {"type": "string", "external_web_access": true}}
            }
        }]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-latest", &client_key())
        .expect("sanitized 4.5 request");
    let body = Value::Object(encoded.body().clone());
    for pointer in [
        "/prompt_cache_retention",
        "/safety_identifier",
        "/presence_penalty",
        "/presencePenalty",
        "/frequency_penalty",
        "/frequencyPenalty",
        "/stop",
        "/external_web_access",
        "/metadata/external_web_access",
        "/tools/0/external_web_access",
        "/tools/0/parameters/properties/q/external_web_access",
    ] {
        assert_eq!(body.pointer(pointer), None, "field survived at {pointer}");
    }

    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "logprobs": true,
        "top_logprobs": 5
    }));
    let encoded = GrokResponsesRequest::encode(&request, "grok-4.20-reasoning", &client_key())
        .expect("sanitized 4.20 request");
    let body = Value::Object(encoded.body().clone());
    assert_eq!(
        body.pointer("/model"),
        Some(&json!("grok-4.20-0309-reasoning"))
    );
    assert_eq!(body.pointer("/logprobs"), None);
    assert_eq!(body.pointer("/top_logprobs"), None);
}

#[test]
fn external_web_access_false_should_strip_only_the_unsupported_field() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "tools": [{
            "type": "web_search",
            "external_web_access": false
        }],
        "tool_choice": {"type": "web_search"}
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("web search survives field sanitization");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/tools/0"),
        Some(&json!({"type": "web_search"}))
    );
    assert_eq!(
        body.pointer("/tool_choice"),
        Some(&json!({"type": "web_search"}))
    );
}

#[test]
fn encoder_should_reject_non_string_build_include_values() {
    for include in [json!("reasoning.encrypted_content"), json!([1])] {
        let request = raw_request(json!({
            "model": "client-model",
            "input": "hello",
            "include": include
        }));
        let error = GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
            .expect_err("invalid include");
        assert!(matches!(
            error,
            GrokRequestEncodeError::InvalidRequestField { field: "include" }
        ));
    }
}

#[test]
fn encoder_should_strip_openai_service_tier_before_grok_build() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "service_tier": "priority"
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
        .expect("sanitized request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/service_tier"), None);
}

#[test]
fn account_identity_should_be_removed_without_touching_prompt_content() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "account_id and x-userid are ordinary prompt text"
            }]
        }],
        "authorization": "Bearer attacker",
        "account_id": "attacker-account",
        "user_id": "attacker-user",
        "team_id": "attacker-team",
        "conversation_id": "attacker-conversation",
        "previous_response_id": "attacker-response",
        "metadata": {
            "accountId": "nested-attacker-account",
            "session_id": "nested-attacker-session",
            "application_tag": "preserve-me"
        }
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
        .expect("sanitized request");
    let body = Value::Object(encoded.body().clone());

    for pointer in [
        "/authorization",
        "/account_id",
        "/user_id",
        "/team_id",
        "/conversation_id",
        "/previous_response_id",
        "/metadata/accountId",
        "/metadata/session_id",
    ] {
        assert_eq!(
            body.pointer(pointer),
            None,
            "identity survived at {pointer}"
        );
    }
    assert_eq!(
        body.pointer("/input/0/content/0/text"),
        Some(&json!("account_id and x-userid are ordinary prompt text"))
    );
    assert_eq!(
        body.pointer("/metadata/application_tag"),
        Some(&json!("preserve-me"))
    );
}

#[test]
fn request_debug_should_not_expose_prompt_or_unknown_values() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "private prompt",
        "future_secret_shaped_value": "must-not-leak"
    }));
    let encoded =
        GrokResponsesRequest::encode(&request, "grok-routed", &client_key()).expect("raw request");
    let debug = format!("{encoded:?}");

    assert!(!debug.contains("private prompt"));
    assert!(!debug.contains("must-not-leak"));
}

#[test]
fn encoder_should_remove_retired_gateway_provider_options_before_xai_conversion() {
    let request = raw_request(Value::Object(Map::from_iter([
        ("model".to_owned(), json!("client")),
        ("input".to_owned(), json!("hello")),
        (
            "provider_options".to_owned(),
            json!({
                "version": "future-version",
                "providers": {"xai": {"transport": "websocket", "turn_index": "7"}}
            }),
        ),
        ("future_official_field".to_owned(), json!({"keep": true})),
    ])));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-routed", &client_key()).expect("request");

    assert!(encoded.body().get("provider_options").is_none());
    assert_eq!(
        encoded.body().get("future_official_field"),
        Some(&json!({"keep": true}))
    );
}

#[test]
fn explicit_session_should_be_tenant_isolated_and_stable_across_turns() {
    let first = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": [{"role": "user", "content": "first"}]
    }));
    let second = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": [
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "second"}
        ]
    }));
    let key_a = client_key();
    let key_b = ClientApiKeyId::new("key_xai_other_tenant").expect("client key id");

    let first_a = GrokResponsesRequest::encode(&first, "grok-4.5", &key_a).expect("first");
    let second_a = GrokResponsesRequest::encode(&second, "grok-4.5", &key_a).expect("second");
    let first_b = GrokResponsesRequest::encode(&first, "grok-4.5", &key_b).expect("other");

    assert_eq!(first_a.session_id(), second_a.session_id());
    assert_eq!(first_a.affinity(), second_a.affinity());
    assert_ne!(first_a.session_id(), first_b.session_id());
    assert_eq!(
        first_a
            .body()
            .get("prompt_cache_key")
            .and_then(Value::as_str),
        first_a.session_id()
    );
}

#[test]
fn explicit_session_should_enable_the_noop_native_cache_route() {
    let request = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": [{"role": "user", "content": "first"}]
    }));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-4.5", &client_key()).expect("request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/tools"),
        Some(&json!([{"type": "web_search"}, {"type": "x_search"}]))
    );
    assert_eq!(body.pointer("/tool_choice"), Some(&json!("none")));
}

#[test]
fn explicit_session_should_add_only_internal_x_search_to_client_tool_requests() {
    let request = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": [{"role": "user", "content": "first"}],
        "tools": [{
            "type": "function",
            "name": "read_file",
            "parameters": {"type": "object"}
        }],
        "tool_choice": "auto"
    }));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-4.5", &client_key()).expect("request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/tools/0"),
        Some(&json!({
            "type": "function",
            "name": "read_file",
            "parameters": {"type": "object"}
        }))
    );
    assert_eq!(body.pointer("/tools/1"), Some(&json!({"type": "x_search"})));
    assert_eq!(body.pointer("/tool_choice"), Some(&json!("auto")));
}

#[test]
fn function_parameters_should_remove_only_nullable_object_roots() {
    let request = raw_request(json!({
        "model": "client",
        "input": "hello",
        "tools": [{
            "type": "function",
            "name": "automation_update",
            "parameters": {
                "anyOf": [
                    {
                        "type": "object",
                        "properties": {
                            "title": {
                                "anyOf": [{"type": "string"}, {"type": "null"}]
                            }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    },
                    {"type": "null"}
                ]
            }
        }]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("nullable object root");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/tools/0/parameters/type"),
        Some(&json!("object"))
    );
    assert_eq!(body.pointer("/tools/0/parameters/anyOf"), None);
    assert_eq!(
        body.pointer("/tools/0/parameters/properties/title/anyOf"),
        Some(&json!([{"type": "string"}, {"type": "null"}]))
    );
}

#[test]
fn function_parameters_should_default_missing_or_null_schemas() {
    let request = raw_request(json!({
        "model": "client",
        "input": "hello",
        "tools": [
            {"type": "function", "name": "lookup"},
            {"type": "function", "name": "wait", "parameters": null}
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("default function schemas");
    let body = Value::Object(encoded.body().clone());
    for index in 0..2 {
        assert_eq!(
            body.pointer(&format!("/tools/{index}/parameters")),
            Some(&json!({"type": "object", "properties": {}}))
        );
    }
}

#[test]
fn function_parameters_should_keep_local_object_refs_after_removing_null() {
    let request = raw_request(json!({
        "model": "client",
        "input": "hello",
        "tools": [{
            "type": "function",
            "name": "lookup",
            "parameters": {
                "$defs": {
                    "Args": {
                        "type": "object",
                        "properties": {"query": {"type": "string"}}
                    }
                },
                "anyOf": [{"$ref": "#/$defs/Args"}, {"type": "null"}]
            }
        }]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("nullable local object ref");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/tools/0/parameters/type"),
        Some(&json!("object"))
    );
    assert_eq!(
        body.pointer("/tools/0/parameters/anyOf"),
        Some(&json!([{"$ref": "#/$defs/Args"}]))
    );
}

#[test]
fn function_parameters_should_report_the_invalid_nullable_root_field() {
    let request = raw_request(json!({
        "model": "client",
        "input": "hello",
        "tools": [{
            "type": "function",
            "name": "invalid",
            "parameters": {
                "anyOf": [{"type": "object"}, {"type": "string"}, {"type": "null"}]
            }
        }]
    }));

    assert_eq!(
        GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
            .expect_err("nullable non-object root"),
        GrokRequestEncodeError::InvalidRequestField {
            field: "tools[].parameters"
        }
    );
}

#[test]
fn explicit_session_should_add_x_search_after_codex_additional_tools_normalization() {
    let request = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "tool_choice": "auto",
        "input": [
            {
                "type": "additional_tools",
                "role": "developer",
                "tools": [
                    {"type": "custom", "name": "apply_patch"},
                    {
                        "type": "function",
                        "name": "read_file",
                        "parameters": {"type": "object"}
                    }
                ]
            },
            {"type": "message", "role": "user", "content": "first"}
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("Codex additional tools");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/input/0/type"), Some(&json!("message")));
    assert_eq!(body.pointer("/input/0/role"), Some(&json!("user")));
    assert_eq!(body.pointer("/input/1"), None);
    assert_eq!(body.pointer("/tools/0/name"), Some(&json!("apply_patch")));
    assert_eq!(
        body.pointer("/tools/0/parameters/required"),
        Some(&json!(["patch"]))
    );
    assert_eq!(body.pointer("/tools/1/name"), Some(&json!("read_file")));
    assert_eq!(body.pointer("/tools/2"), Some(&json!({"type": "x_search"})));
    assert_eq!(body.pointer("/tool_choice"), Some(&json!("auto")));
}

#[test]
fn additional_tools_should_keep_top_level_definition_and_drop_unsupported_types() {
    let request = raw_request(json!({
        "model": "client",
        "tools": [
            {
                "type": "function",
                "name": "existing",
                "description": "top-level wins",
                "parameters": {"type": "object"}
            },
            {"type": "image_generation", "model": "gpt-image-2"}
        ],
        "tool_choice": "auto",
        "input": [
            {
                "type": "additional_tools",
                "tools": [
                    {
                        "type": "function",
                        "name": "existing",
                        "description": "carrier duplicate",
                        "parameters": {"type": "object"}
                    },
                    {"type": "function", "name": "wait"},
                    {"type": "future_client_tool", "name": "drop-me"}
                ]
            },
            {"type": "message", "role": "user", "content": "hello"}
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("promoted additional tools");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/input/0/role"), Some(&json!("user")));
    assert_eq!(body.pointer("/input/1"), None);
    assert_eq!(body.pointer("/tools/0/name"), Some(&json!("existing")));
    assert_eq!(
        body.pointer("/tools/0/description"),
        Some(&json!("top-level wins"))
    );
    assert_eq!(body.pointer("/tools/1/name"), Some(&json!("wait")));
    assert_eq!(body.pointer("/tools/2"), None);
    assert_eq!(body.pointer("/tool_choice"), Some(&json!("auto")));
}

#[test]
fn explicit_session_should_drop_xai_unsupported_allowed_tools_choice() {
    let request = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": "first",
        "tools": [{
            "type": "function",
            "name": "read_file",
            "parameters": {"type": "object"}
        }],
        "tool_choice": {
            "type": "allowed_tools",
            "mode": "auto",
            "tools": [{"type": "function", "name": "read_file"}]
        }
    }));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-4.5", &client_key()).expect("request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/tool_choice"), None);
    assert_eq!(body.pointer("/tools/1"), Some(&json!({"type": "x_search"})));
}

#[test]
fn explicit_session_should_not_duplicate_existing_native_cache_tools() {
    let request = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": "first",
        "tools": [
            {"type": "web_search", "filters": {"allowed_domains": ["example.com"]}},
            {"type": "x_search"},
            {
                "type": "function",
                "name": "read_file",
                "parameters": {"type": "object"}
            }
        ],
        "tool_choice": "required"
    }));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-4.5", &client_key()).expect("request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/tools/0/type"), Some(&json!("web_search")));
    assert_eq!(body.pointer("/tools/1/type"), Some(&json!("x_search")));
    assert_eq!(body.pointer("/tools/2/name"), Some(&json!("read_file")));
    assert_eq!(body.pointer("/tools/3"), None);
    assert_eq!(body.pointer("/tool_choice"), Some(&json!("required")));
}

#[test]
fn soft_session_should_not_enable_the_native_cache_route() {
    let request = raw_request(json!({
        "model": "client",
        "instructions": "stable system",
        "input": [{"role": "user", "content": "first prompt"}]
    }));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-4.5", &client_key()).expect("request");

    assert!(!encoded.body().contains_key("tools"));
}

#[test]
fn soft_session_should_follow_the_first_user_anchor() {
    let first = raw_request(json!({
        "model": "client",
        "instructions": "stable system",
        "input": [{"type": "message", "role": "user", "content": "first prompt"}]
    }));
    let later = raw_request(json!({
        "model": "client",
        "instructions": "stable system",
        "input": [
            {"type": "message", "role": "user", "content": "first prompt"},
            {"type": "message", "role": "assistant", "content": "answer"},
            {"type": "message", "role": "user", "content": "next prompt"}
        ]
    }));

    let first = GrokResponsesRequest::encode(&first, "grok-4.5", &client_key()).expect("first");
    let later = GrokResponsesRequest::encode(&later, "grok-4.5", &client_key()).expect("later");

    assert_eq!(first.session_id(), later.session_id());
    assert_eq!(first.affinity(), later.affinity());
}

#[test]
fn response_format_and_reasoning_parts_should_match_build_wire_shape() {
    let request = raw_request(json!({
        "model": "client",
        "input": [{
            "type": "reasoning",
            "content": [{"text": "summary"}]
        }],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "answer",
                "strict": true,
                "schema": {"type": "object"}
            }
        }
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("normalized request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/response_format"), None);
    assert_eq!(
        body.pointer("/text/format/type"),
        Some(&json!("json_schema"))
    );
    assert_eq!(body.pointer("/text/format/name"), Some(&json!("answer")));
    assert_eq!(
        body.pointer("/input/0/content/0/type"),
        Some(&json!("reasoning_text"))
    );
}

#[test]
fn tool_declarations_should_flatten_and_emulate_codex_tool_shapes() {
    let request = raw_request(json!({
        "model": "client",
        "input": "use tools",
        "parallel_tool_calls": true,
        "tools": [
            {
                "type": "namespace",
                "name": "workspace",
                "tools": [{
                    "type": "function",
                    "name": "read",
                    "parameters": {"type": "object"}
                }]
            },
            {"type": "custom", "name": "render", "format": {"type": "text"}},
            {"type": "apply_patch"},
            {"type": "local_shell"},
            {
                "type": "function",
                "name": "deferred_lookup",
                "description": "deferred",
                "defer_loading": true,
                "parameters": {"type": "object"}
            },
            {"type": "tool_search", "execution": "client"}
        ],
        "tool_choice": {"type": "function", "name": "read", "namespace": "workspace"}
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("normalized tools");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/parallel_tool_calls"), Some(&json!(false)));
    assert_eq!(
        body.pointer("/tools/0/name"),
        Some(&json!("workspace__read"))
    );
    assert_eq!(body.pointer("/tools/1/name"), Some(&json!("render")));
    assert_eq!(
        body.pointer("/tools/2/name"),
        Some(&json!("xai_proxy_apply_patch"))
    );
    assert_eq!(body.pointer("/tools/3/type"), Some(&json!("shell")));
    assert_eq!(
        body.pointer("/tools/3/environment/type"),
        Some(&json!("local"))
    );
    assert_eq!(
        body.pointer("/tools/4/name"),
        Some(&json!("xai_proxy_tool_search"))
    );
    assert_eq!(
        body.pointer("/tool_choice/name"),
        Some(&json!("workspace__read"))
    );
    assert_eq!(body.pointer("/tool_choice/namespace"), None);
}

#[test]
fn custom_apply_patch_declaration_should_use_patch_parameter() {
    let request = raw_request(json!({
        "model": "client",
        "input": "edit",
        "tools": [
            {
                "type": "custom",
                "name": "apply_patch",
                "description": "This is a FREEFORM tool, so do not wrap the patch in JSON."
            },
            {"type": "custom", "name": "render"},
            {
                "type": "function",
                "name": "apply_patch",
                "parameters": {
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"]
                }
            }
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("custom apply_patch declaration");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/tools/0/parameters/properties"),
        Some(&json!({"patch": {"type": "string"}}))
    );
    assert_eq!(
        body.pointer("/tools/0/parameters/required"),
        Some(&json!(["patch"]))
    );
    assert_eq!(
        body.pointer("/tools/0/description"),
        Some(&json!(
            "The apply_patch tool edits files using Codex patch format. Provide the complete raw patch text in the patch string field."
        ))
    );
    assert_eq!(
        body.pointer("/tools/1/parameters/properties"),
        Some(&json!({"input": {"type": "string"}}))
    );
    assert_eq!(
        body.pointer("/tools/2/parameters/properties"),
        Some(&json!({"command": {"type": "string"}}))
    );
}

#[test]
fn hosted_tool_choice_should_preserve_normalized_web_search_choice() {
    let request = raw_request(json!({
        "model": "client",
        "input": "search",
        "tools": [
            {"type": "web_search_preview", "allowed_domains": ["example.com"]},
            {"type": "x_search"}
        ],
        "tool_choice": {"type": "web_search_preview"}
    }));

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-4.5", &client_key()).expect("hosted choice");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/tool_choice"),
        Some(&json!({"type": "web_search"}))
    );
    assert_eq!(
        body.pointer("/tools"),
        Some(&json!([
            {
                "type": "web_search",
                "filters": {"allowed_domains": ["example.com"]}
            },
            {"type": "x_search"}
        ]))
    );
}

#[test]
fn history_should_rebuild_codex_calls_outputs_shell_and_private_fields() {
    let request = raw_request(json!({
        "model": "client",
        "tools": [
            {"type": "custom", "name": "render"},
            {"type": "apply_patch"},
            {"type": "local_shell"}
        ],
        "input": [
            {"type": "message", "role": "assistant", "id": "msg_1", "content": [
                {"type": "output_text", "text": "done"},
                {"type": "refusal", "refusal": "no"}
            ]},
            {"type": "custom_tool_call", "call_id": "custom_1", "name": "render", "input": "raw"},
            {"type": "custom_tool_call_output", "call_id": "custom_1", "output": {"ok": true}, "status": "completed"},
            {"type": "apply_patch_call", "call_id": "patch_1", "operation": {"type": "delete_file", "path": "old.txt"}},
            {"type": "apply_patch_call_output", "call_id": "patch_1", "status": "completed", "output": "deleted"},
            {"type": "local_shell_call", "call_id": "shell_1", "action": {"type": "exec", "command": ["printf", "%s", "a b"], "working_directory": "/tmp"}},
            {"type": "local_shell_call_output", "call_id": "shell_1", "status": "failed", "output": "failure"},
            {"type": "reasoning", "id": "reason_1", "status": "completed", "summary": [{"type": "summary_text", "text": "brief", "phase": "drop"}]},
            {"type": "future_codex_item", "id": "future_1", "status": "completed"}
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("history normalization");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/input/0/content"), Some(&json!("done\nno")));
    assert_eq!(body.pointer("/input/0/id"), Some(&json!("msg_1")));
    assert_eq!(body.pointer("/input/1/type"), Some(&json!("function_call")));
    assert_eq!(
        body.pointer("/input/1/arguments"),
        Some(&json!("{\"input\":\"raw\"}"))
    );
    assert_eq!(
        body.pointer("/input/2/output"),
        Some(&json!("{\"ok\":true}"))
    );
    assert_eq!(
        body.pointer("/input/3/name"),
        Some(&json!("xai_proxy_apply_patch"))
    );
    assert_eq!(
        body.pointer("/input/4/output"),
        Some(&json!("Apply patch status: completed\ndeleted"))
    );
    assert_eq!(body.pointer("/input/5/type"), Some(&json!("shell_call")));
    assert_eq!(
        body.pointer("/input/5/action/commands/0"),
        Some(&json!("cd /tmp && printf %s 'a b'"))
    );
    assert_eq!(
        body.pointer("/input/6/output/0/outcome/exit_code"),
        Some(&json!(1))
    );
    assert_eq!(body.pointer("/input/7/status"), None);
    assert_eq!(body.pointer("/input/7/summary/0/phase"), None);
    assert_eq!(body.pointer("/input/8/role"), Some(&json!("developer")));
}

#[test]
fn history_rebuild_should_preserve_unknown_fields_and_strip_grok_internal_keys() {
    let request = raw_request(json!({
        "model": "client",
        "input": [
            {
                "type": "message", "role": "assistant", "status": "completed",
                "phase": "final_answer",
                "internal_chat_message_metadata_passthrough": {"turn_id": "t1"},
                "future_message_field": {"keep": true},
                "content": [{"type": "output_text", "text": "hi"}]
            },
            {
                "type": "function_call", "id": "fc_1", "call_id": "call_1",
                "name": "read_file", "arguments": "{}",
                "future_call_field": 7,
                "internal_chat_message_metadata_passthrough": {"turn_id": "t1"}
            },
            {
                "type": "function_call_output", "id": "fco_1", "call_id": "call_1",
                "output": "ok", "future_output_field": true,
                "internal_chat_message_metadata_passthrough": {"turn_id": "t1"}
            },
            {
                "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_2",
                "name": "render", "input": "raw", "status": "completed",
                "future_custom_field": "keep",
                "internal_chat_message_metadata_passthrough": {"turn_id": "t1"}
            }
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("history normalization");
    let body = Value::Object(encoded.body().clone());

    for pointer in [
        "/input/0/phase",
        "/input/0/internal_chat_message_metadata_passthrough",
        "/input/1/internal_chat_message_metadata_passthrough",
        "/input/2/internal_chat_message_metadata_passthrough",
        "/input/3/internal_chat_message_metadata_passthrough",
        "/input/3/input",
    ] {
        assert_eq!(body.pointer(pointer), None, "field survived at {pointer}");
    }
    assert_eq!(
        body.pointer("/input/0/future_message_field"),
        Some(&json!({"keep": true}))
    );
    assert_eq!(body.pointer("/input/0/status"), Some(&json!("completed")));
    assert_eq!(body.pointer("/input/0/content"), Some(&json!("hi")));
    assert_eq!(body.pointer("/input/1/id"), Some(&json!("fc_1")));
    assert_eq!(body.pointer("/input/1/future_call_field"), Some(&json!(7)));
    assert_eq!(
        body.pointer("/input/2/future_output_field"),
        Some(&json!(true))
    );
    assert_eq!(body.pointer("/input/2/output"), Some(&json!("ok")));
    assert_eq!(body.pointer("/input/3/type"), Some(&json!("function_call")));
    assert_eq!(
        body.pointer("/input/3/future_custom_field"),
        Some(&json!("keep"))
    );
    assert_eq!(
        body.pointer("/input/3/arguments"),
        Some(&json!("{\"input\":\"raw\"}"))
    );
}

#[test]
fn history_sanitizer_should_only_strip_known_grok_injection_sites() {
    let request = raw_request(json!({
        "model": "client",
        "input": [
            {"type": "mcp_list_tools", "id": "ml_1", "server_label": "srv", "tools": [
                {
                    "name": "deploy",
                    "input_schema": {
                        "properties": {"phase": {"type": "string"}},
                        "required": ["phase"]
                    }
                }
            ]},
            {
                "type": "mcp_call", "id": "mc_1", "name": "deploy",
                "server_label": "srv", "arguments": "{}",
                "output": {"result": null, "phase": "keep"}
            },
            {
                "type": "shell_call", "id": "sh_1", "call_id": "call_9",
                "status": "completed",
                "action": {
                    "commands": ["pwd"],
                    "timeout_ms": null,
                    "internal_chat_message_metadata_passthrough": {"turn_id": "t1"}
                }
            }
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("history normalization");
    let body = Value::Object(encoded.body().clone());

    // 工具 schema 里恰好叫 phase 的属性与语义 null 不再被误删。
    assert_eq!(
        body.pointer("/input/0/tools/0/input_schema/properties/phase"),
        Some(&json!({"type": "string"}))
    );
    assert_eq!(body.pointer("/input/1/output/result"), Some(&Value::Null));
    assert_eq!(body.pointer("/input/1/output/phase"), Some(&json!("keep")));
    // shell_call action 是已知注入点：内部键与 null 占位字段仍被剥离。
    assert_eq!(
        body.pointer("/input/2/action/commands/0"),
        Some(&json!("pwd"))
    );
    assert_eq!(body.pointer("/input/2/action/timeout_ms"), None);
    assert_eq!(
        body.pointer("/input/2/action/internal_chat_message_metadata_passthrough"),
        None
    );
}

#[test]
fn custom_apply_patch_history_should_wrap_raw_input_in_patch_field() {
    let patch = concat!(
        "*** Begin Patch\n",
        "*** Update File: src/lib.rs\n",
        "@@\n",
        "-let path = \"old\\\\name\";\n",
        "+let path = \"new\\\\name\";\n",
        "*** End Patch\n",
    );
    let request = raw_request(json!({
        "model": "client",
        "tools": [{"type": "custom", "name": "apply_patch"}],
        "input": [
            {
                "type": "custom_tool_call",
                "call_id": "patch_custom_1",
                "name": "apply_patch",
                "input": patch
            },
            {
                "type": "custom_tool_call_output",
                "call_id": "patch_custom_1",
                "output": "Done!"
            }
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("custom apply_patch history");
    let body = Value::Object(encoded.body().clone());
    let arguments = body
        .pointer("/input/0/arguments")
        .and_then(Value::as_str)
        .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
        .expect("function arguments");

    assert_eq!(arguments, json!({"patch": patch}));
    assert_eq!(
        body.pointer("/input/1/type"),
        Some(&json!("function_call_output"))
    );
}

#[test]
fn compaction_history_should_become_plaintext_user_continuation_in_place() {
    let request = raw_request(json!({
        "model": "client",
        "input": [
            {"type": "message", "role": "user", "content": "before compaction"},
            {
                "type": "compaction",
                "encrypted_content": "Repository state and pending work."
            },
            {"type": "message", "role": "user", "content": "continue"}
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("compaction continuation");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/input"),
        Some(&json!([
            {"type": "message", "role": "user", "content": "before compaction"},
            {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": concat!(
                        "This session is being continued from a previous conversation that ran out of context. ",
                        "The summary below covers the earlier portion of the conversation.\n\n",
                        "Repository state and pending work."
                    )
                }]
            },
            {"type": "message", "role": "user", "content": "continue"}
        ]))
    );
}

#[test]
fn structured_compaction_history_should_restore_reasoning_ciphertext_and_visible_summary() {
    for item_type in ["compaction", "compaction_summary"] {
        let request = raw_request(json!({
            "model": "client",
            "input": [
                {
                    "type": item_type,
                    "id": "cmp_1",
                    "status": "completed",
                    "encrypted_content": "grok-encrypted-state",
                    "summary": [
                        {"type": "summary_text", "text": "first part"},
                        {"type": "summary_text", "text": "second part"}
                    ]
                },
                {"type": "message", "role": "user", "content": "continue"}
            ]
        }));

        let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
            .expect("structured compaction continuation");
        let body = Value::Object(encoded.body().clone());

        assert_eq!(
            body.pointer("/input"),
            Some(&json!([
                {
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "grok-encrypted-state"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "<conversation_summary>\nfirst part\nsecond part\n</conversation_summary>"
                    }]
                },
                {"type": "message", "role": "user", "content": "continue"}
            ]))
        );
    }
}

#[test]
fn malformed_compaction_history_should_be_dropped() {
    for encrypted_content in [
        None,
        Some(json!(null)),
        Some(json!("")),
        Some(json!(" \n\t")),
        Some(json!(42)),
        Some(json!({"summary": "not a string"})),
    ] {
        let mut compaction =
            Map::from_iter([("type".to_owned(), Value::String("compaction".to_owned()))]);
        if let Some(encrypted_content) = encrypted_content {
            compaction.insert("encrypted_content".to_owned(), encrypted_content);
        }
        let request = raw_request(json!({
            "model": "client",
            "input": [Value::Object(compaction)]
        }));
        let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
            .expect("malformed compaction item is omitted");
        assert_eq!(encoded.body().get("input"), Some(&json!([])));
    }
}

#[test]
fn tool_search_history_should_load_returned_tools_at_the_original_turn() {
    let request = raw_request(json!({
        "model": "client",
        "tools": [{"type": "tool_search", "execution": "client"}],
        "input": [
            {"type": "tool_search_call", "execution": "client", "call_id": "search_1", "arguments": {"goal": "shipping"}},
            {"type": "tool_search_output", "execution": "client", "call_id": "search_1", "tools": [{
                "type": "namespace",
                "name": "shipping",
                "tools": [{"type": "function", "name": "track", "parameters": {"type": "object"}}]
            }]}
        ]
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("tool search history");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        body.pointer("/input/0/name"),
        Some(&json!("xai_proxy_tool_search"))
    );
    assert_eq!(
        body.pointer("/input/1/type"),
        Some(&json!("function_call_output"))
    );
    assert_eq!(
        body.pointer("/tools/0/name"),
        Some(&json!("shipping__track"))
    );
    assert_eq!(
        body.pointer("/tools/1/name"),
        Some(&json!("xai_proxy_tool_search"))
    );
}

#[test]
fn unsupported_tools_should_be_filtered_with_their_orphaned_choice() {
    let request = raw_request(json!({
        "model": "client",
        "input": "x",
        "tools": [{"type": "future_tool"}],
        "tool_choice": {"type": "future_tool"}
    }));
    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("unsupported tool filter");
    assert_eq!(encoded.body().get("tools"), None);
    assert_eq!(encoded.body().get("tool_choice"), None);

    let request = raw_request(json!({
        "model": "client",
        "input": "x",
        "tools": [{"type": "function", "name": "kept"}],
        "tool_choice": {"type": "function", "name": "missing"}
    }));
    let encoded = GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
        .expect("orphaned function choice filter");
    let body = Value::Object(encoded.body().clone());
    assert_eq!(body.pointer("/tools/0/name"), Some(&json!("kept")));
    assert_eq!(encoded.body().get("tool_choice"), None);
}

#[test]
fn ambiguous_tool_contracts_should_fail_before_upstream_io() {
    for (body, field) in [
        (
            json!({"model": "client", "input": "x", "tools": [
                {"type": "tool_search", "execution": "client"},
                {"type": "tool_search", "execution": "server"}
            ]}),
            "tools",
        ),
        (
            json!({"model": "client", "input": "x", "tools": [
                {"type": "shell"}, {"type": "local_shell"}
            ]}),
            "tools",
        ),
        (
            json!({"model": "client", "input": [{
                "type": "apply_patch_call",
                "call_id": "patch_1",
                "operation": {"type": "update_file", "path": "a.txt"}
            }]}),
            "input",
        ),
        (
            json!({"model": "client", "input": [{"type": "compaction_trigger"}]}),
            "input",
        ),
    ] {
        let request = raw_request(body);
        assert_eq!(
            GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
                .expect_err("invalid tool contract"),
            GrokRequestEncodeError::InvalidRequestField { field }
        );
    }
}
