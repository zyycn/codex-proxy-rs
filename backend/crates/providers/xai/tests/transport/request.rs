use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, ProtocolPayload, ProviderOptions,
};
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};

use provider_xai::{GrokRequestEncodeError, GrokResponsesRequest};

fn raw_request(body: Value) -> GenerateRequest {
    let Value::Object(body) = body else {
        panic!("request fixture must be an object");
    };
    GenerateRequest::from_protocol_payload(
        Vec::new(),
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
fn encoder_should_strip_openai_only_client_metadata_before_grok_build() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "hello",
        "client_metadata": {
            "x-openai-subagent": "review",
            "application_tag": "preserve-me"
        }
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
        .expect("sanitized request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/client_metadata/x-openai-subagent"), None);
    assert_eq!(
        body.pointer("/client_metadata/application_tag"),
        Some(&json!("preserve-me"))
    );
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
fn typed_projection_should_not_exist_as_a_second_request_path() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("private prompt".to_owned())],
    )
    .expect("message");
    let request = GenerateRequest::new(vec![message]).expect("typed operation");

    assert_eq!(
        GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
            .expect_err("missing raw payload must fail"),
        GrokRequestEncodeError::InvalidProtocolPayload
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
fn provider_options_should_only_select_the_supported_transport() {
    for raw_options in [
        json!({"schema_version": 1, "conversation_id": "attacker"}),
        json!({"schema_version": 1, "transport": "websocket"}),
    ] {
        let mut provider_options = ProviderOptions::new();
        provider_options
            .insert(
                "xai",
                raw_options
                    .as_object()
                    .cloned()
                    .expect("provider options object"),
            )
            .expect("provider options");
        let request = raw_request(json!({"model": "client", "input": "hello"}))
            .with_provider_options(provider_options);

        assert!(matches!(
            GrokResponsesRequest::encode(&request, "grok-routed", &client_key()),
            Err(GrokRequestEncodeError::UnsupportedProviderOption
                | GrokRequestEncodeError::InvalidProviderOptions)
        ));
    }

    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert(
            "xai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                ("transport".to_owned(), json!("http_sse")),
                ("turn_index".to_owned(), json!("7")),
            ]),
        )
        .expect("provider options");
    let request = raw_request(json!({"model": "client", "input": "hello"}))
        .with_provider_options(provider_options);

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-routed", &client_key()).expect("request");
    assert_eq!(encoded.turn_index(), Some("7"));
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
fn explicit_session_should_preserve_a_client_tool_contract() {
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
        body.pointer("/tools"),
        Some(&json!([{
            "type": "function",
            "name": "read_file",
            "parameters": {"type": "object"}
        }]))
    );
    assert_eq!(body.pointer("/tool_choice"), Some(&json!("auto")));
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
fn hosted_tool_choice_should_narrow_visible_tools_and_require_the_selected_kind() {
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

    assert_eq!(body.pointer("/tool_choice"), Some(&json!("required")));
    assert_eq!(
        body.pointer("/tools"),
        Some(&json!([{
            "type": "web_search",
            "filters": {"allowed_domains": ["example.com"]}
        }]))
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
            {"type": "message", "role": "assistant", "id": "drop", "content": [
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
    assert_eq!(body.pointer("/input/0/id"), None);
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
fn unsupported_or_ambiguous_tool_contracts_should_fail_before_upstream_io() {
    for body in [
        json!({"model": "client", "input": "x", "tools": [{"type": "future_tool"}]}),
        json!({"model": "client", "input": "x", "tools": [
            {"type": "tool_search", "execution": "client"},
            {"type": "tool_search", "execution": "server"}
        ]}),
        json!({"model": "client", "input": "x", "tools": [
            {"type": "shell"}, {"type": "local_shell"}
        ]}),
        json!({"model": "client", "input": [{
            "type": "apply_patch_call",
            "call_id": "patch_1",
            "operation": {"type": "update_file", "path": "a.txt"}
        }]}),
        json!({"model": "client", "input": [{"type": "compaction_trigger"}]}),
    ] {
        let request = raw_request(body);
        assert_eq!(
            GrokResponsesRequest::encode(&request, "grok-4.5", &client_key())
                .expect_err("invalid tool contract"),
            GrokRequestEncodeError::InvalidRequestNormalization
        );
    }
}
