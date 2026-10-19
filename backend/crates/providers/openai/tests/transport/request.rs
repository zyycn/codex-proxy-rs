//! 验证生成请求编码的模型、位置覆盖与不透明字段保留

use chrono::{Datelike as _, Utc};
use chrono_tz::America::New_York;
use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use serde_json::{Map, Value, json};

use provider_openai::encode_generate_request;
use provider_openai::transport::profile::CodexRequestLocation;

#[test]
fn encoder_should_use_the_configured_location_for_tools_and_environment_dates() {
    let location: CodexRequestLocation = serde_json::from_value(json!({
        "country": "NZ", "region": "Auckland", "city": "Auckland", "timezone": "Pacific/Auckland"
    }))
    .expect("configured location");
    let body = json!({
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": "<environment_context><current_date>2020-01-01</current_date><timezone>UTC</timezone></environment_context>"}],
            "internal_chat_message_metadata_passthrough": {
                "content_item_kinds": ["environments.environment_context"],
                "create_time": 1789293131.822
            }
        }],
        "tools": [
            {"type": "web_search", "user_location": {"type": "approximate", "country": "US"}},
            {"type": "web_search_preview"}
        ]
    }).as_object().expect("request object").clone();
    let before = Utc::now()
        .with_timezone(&location.timezone)
        .format("%Y-%m-%d")
        .to_string();
    let encoded =
        encode_generate_request(&request(body), "gpt-test", Some(&location)).expect("encode");
    let after = Utc::now()
        .with_timezone(&location.timezone)
        .format("%Y-%m-%d")
        .to_string();
    let encoded = Value::Object(encoded.body().clone());
    let environment = encoded
        .pointer("/input/0/content/0/text")
        .and_then(Value::as_str)
        .expect("environment");
    assert!(environment.contains("<timezone>Pacific/Auckland</timezone>"));
    assert!(
        [before, after]
            .iter()
            .any(|date| environment.contains(&format!("<current_date>{date}</current_date>")))
    );
    for index in 0..2 {
        assert_eq!(
            encoded.pointer(&format!("/tools/{index}/user_location")),
            Some(&json!({
                "type": "approximate", "country": "NZ", "region": "Auckland", "city": "Auckland", "timezone": "Pacific/Auckland"
            }))
        );
    }
    assert_eq!(
        encoded.pointer("/input/0/internal_chat_message_metadata_passthrough/create_time"),
        Some(&json!(1789293131.822))
    );
}

fn request(body: Map<String, Value>) -> GenerateRequest {
    GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    )
}

#[test]
fn encoder_should_preserve_location_fields_when_no_override_is_configured() {
    let body = json!({
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": "<environment_context><current_date>2020-01-01</current_date><timezone>Asia/Shanghai</timezone></environment_context>"}],
            "internal_chat_message_metadata_passthrough": {
                "content_item_kinds": ["environments.environment_context"],
                "create_time": 1789293131.822
            }
        }],
        "tools": [
            {"type": "web_search", "user_location": {"type": "approximate", "country": "CN", "region": "Shanghai", "city": "Shanghai", "timezone": "Asia/Shanghai"}},
            {"type": "web_search_preview", "user_location": null},
            {"type": "web_search_preview_2025_03_11"},
            {"type": "function", "name": "keep_timezone", "parameters": {}}
        ]
    }).as_object().expect("request object").clone();
    let encoded = encode_generate_request(&request(body.clone()), "gpt-test", None)
        .expect("encode without location override");
    assert_eq!(encoded.body().get("input"), body.get("input"));
    assert_eq!(encoded.body().get("tools"), body.get("tools"));
    assert_eq!(encoded.body().get("model"), Some(&json!("gpt-test")));
}

#[test]
fn encoder_should_align_official_environment_context_without_content_kinds() {
    let environment = "<environment_context>\n  <environments>\n    \
        <environment id=\"local\" primary=\"true\"><cwd>/home/example/project</cwd><shell>bash</shell></environment>\n  \
        </environments>\n  <current_date>2020-01-01</current_date>\n  \
        <timezone>Asia/Shanghai</timezone>\n</environment_context>";
    for metadata in [None, Some(json!({"create_time": 1789293131.822}))] {
        let mut message = json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "Please explain the current environment"},
                {"type": "input_text", "text": environment}
            ]
        });
        if let Some(metadata) = metadata {
            message["internal_chat_message_metadata_passthrough"] = metadata;
        }
        let body = json!({"input": [message]})
            .as_object()
            .expect("request object")
            .clone();
        let before = Utc::now()
            .with_timezone(&New_York)
            .format("%Y-%m-%d")
            .to_string();
        let encoded = encode_generate_request(
            &request(body.clone()),
            "gpt-test",
            Some(&CodexRequestLocation::default()),
        )
        .expect("encode unclassified official context");
        let after = Utc::now()
            .with_timezone(&New_York)
            .format("%Y-%m-%d")
            .to_string();
        let encoded = Value::Object(encoded.body().clone());
        let aligned = encoded
            .pointer("/input/0/content/1/text")
            .and_then(Value::as_str)
            .expect("aligned environment");
        assert!(aligned.contains("<timezone>America/New_York</timezone>"));
        assert!([before, after].iter().any(|date| {
            aligned
                == environment
                    .replace("2020-01-01", date)
                    .replace("Asia/Shanghai", "America/New_York")
        }));
        let original = Value::Object(body.clone());
        assert_eq!(
            encoded.pointer("/input/0/content/0"),
            original.pointer("/input/0/content/0")
        );
        assert_eq!(
            encoded.pointer("/input/0/internal_chat_message_metadata_passthrough"),
            original.pointer("/input/0/internal_chat_message_metadata_passthrough")
        );

        let unchanged = encode_generate_request(&request(body.clone()), "gpt-test", None)
            .expect("encode without override");
        assert_eq!(unchanged.body().get("input"), body.get("input"));
    }
}

#[test]
fn encoder_should_align_desktop_time_context_with_environment_dates() {
    // Desktop Core 0.162.0-alpha.17.2 的应用附加上下文使用 developer role
    let desktop = " <codex_apps_client_time_context><timezone>Asia/Shanghai</timezone>\n\
        <current_date>2020-01-01</current_date>\n\
        Use this client time context for user-facing dates, times, and schedules instead of the execution host's timezone and current date.\
        </codex_apps_client_time_context>\n";
    let environment = "<environment_context><current_date>2020-01-01</current_date><timezone>Asia/Shanghai</timezone></environment_context>";
    for kinds in [
        None,
        Some(json!([
            "developer.text",
            "additional_content.codex_apps_client_time_context"
        ])),
    ] {
        let mut message = json!({
            "type": "message", "role": "developer",
            "content": [
                {"type": "input_text", "text": "Keep Asia/Shanghai in this instruction"},
                {"type": "input_text", "text": desktop}
            ],
            "internal_chat_message_metadata_passthrough": {"create_time": 1789293131.822}
        });
        if let Some(kinds) = kinds {
            message["internal_chat_message_metadata_passthrough"]["content_item_kinds"] = kinds;
        }
        let body = json!({"input": [
            {"role": "user", "content": [{"type": "input_text", "text": environment}]},
            message
        ]})
        .as_object()
        .expect("request object")
        .clone();
        let before = Utc::now()
            .with_timezone(&New_York)
            .format("%Y-%m-%d")
            .to_string();
        let encoded = encode_generate_request(
            &request(body.clone()),
            "gpt-test",
            Some(&CodexRequestLocation::default()),
        )
        .expect("encode Desktop time context");
        let after = Utc::now()
            .with_timezone(&New_York)
            .format("%Y-%m-%d")
            .to_string();
        assert!(
            [before, after].iter().any(|date| {
                let mut expected = body["input"].clone();
                expected[0]["content"][0]["text"] = json!(
                    environment
                        .replace("2020-01-01", date)
                        .replace("Asia/Shanghai", "America/New_York")
                );
                expected[1]["content"][1]["text"] = json!(
                    desktop
                        .replace("2020-01-01", date)
                        .replace("Asia/Shanghai", "America/New_York")
                );
                encoded.body().get("input") == Some(&expected)
            }),
            "both time contexts must agree while preserving other content and metadata"
        );
        let unchanged = encode_generate_request(&request(body.clone()), "gpt-test", None)
            .expect("encode without override");
        assert_eq!(unchanged.body().get("input"), body.get("input"));
    }
}

#[test]
fn encoder_should_preserve_text_outside_desktop_time_context() {
    let desktop = "<codex_apps_client_time_context><timezone>Asia/Shanghai</timezone><current_date>2020-01-01</current_date></codex_apps_client_time_context>";
    let input = json!([
        {"role": "user", "content": [{"type": "input_text", "text": desktop}]},
        {"role": "assistant", "content": [{"type": "output_text", "text": desktop}]},
        {"type": "function_call_output", "call_id": "synthetic", "output": desktop},
        {"role": "developer", "content": [{"type": "input_text", "text": desktop}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["user.text"]}},
        {"role": "developer", "content": [{"type": "input_text", "text": desktop}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["environments.environment_context"]}},
        {"role": "developer", "content": [{"type": "input_text", "text": desktop}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": []}},
        {"role": "developer", "content": [{"type": "input_text", "text": format!("Example:\n{desktop}")}]},
        {"role": "developer", "content": [{"type": "input_text", "text": format!("{desktop}\nKeep this example")}]},
        {"role": "developer", "content": [{"type": "input_text", "text": "<codex_apps_client_time_context><timezone>Asia/Shanghai</codex_apps_client_time_context>"}]},
        {"role": "developer", "content": [{"type": "input_text", "text": "<other_context><timezone>Asia/Shanghai</timezone></other_context>"}]},
        {"role": "developer", "content": [{"type": "output_text", "text": desktop}]},
        {"role": "developer", "content": [{"type": "input_text", "text": "<codex_apps_client_time_context><example><timezone>Asia/Shanghai</timezone><current_date>2020-01-01</current_date></example></codex_apps_client_time_context>"}]}
    ]);
    let encoded = encode_generate_request(
        &request(Map::from_iter([("input".to_owned(), input.clone())])),
        "gpt-test",
        Some(&CodexRequestLocation::default()),
    )
    .expect("encode without changing unrelated content");
    assert_eq!(encoded.body().get("input"), Some(&input));
}

#[test]
fn encoder_should_preserve_classified_text_and_non_user_environment_items() {
    let environment =
        "<environment_context><timezone>Asia/Shanghai</timezone></environment_context>";
    let input = json!([
        {"role": "user", "content": [{"type": "input_text", "text": environment}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["user.text"]}},
        {"role": "user", "content": [{"type": "input_text", "text": environment}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["future.context"]}},
        {"role": "user", "content": [{"type": "input_text", "text": environment}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": []}},
        {"role": "developer", "content": [{"type": "input_text", "text": environment}]},
        {"role": "assistant", "content": [{"type": "output_text", "text": environment}]},
        {"type": "function_call_output", "call_id": "synthetic", "output": environment},
        {"role": "user", "content": [{"type": "input_text", "text": "<environment_context><timezone>Asia/Shanghai</timezone>"}]}
    ]);
    let encoded = encode_generate_request(
        &request(Map::from_iter([("input".to_owned(), input.clone())])),
        "gpt-test",
        Some(&CodexRequestLocation::default()),
    )
    .expect("encode without changing non-environment content");
    assert_eq!(encoded.body().get("input"), Some(&input));
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

    let encoded =
        encode_generate_request(&request, "gpt-routed", None).expect("encode wire payload");

    assert_eq!(encoded.body().get("model"), Some(&json!("gpt-routed")));
    assert!(encoded.body().get("stream").is_none());
    assert_eq!(encoded.body().get("store"), Some(&json!(false)));
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
            encode_generate_request(&request(input), "gpt-routed", None).expect("encoded request");
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

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");
    assert_eq!(encoded.body().get("model"), Some(&json!("gpt-test")));
    assert!(encoded.body().get("stream").is_none());
    assert_eq!(encoded.body().get("store"), Some(&json!(false)));
    let body = Value::Object(encoded.body().clone());
    assert_eq!(body.pointer("/tools/0/strict"), Some(&json!(true)));
    assert_eq!(body.pointer("/reasoning/effort"), Some(&json!("high")));
    assert!(!encoded.force_http_sse);
}

#[test]
fn encoder_should_align_structured_location_fields_without_rewriting_chat_text() {
    let normal_chat = "<environment_context>\n  <current_date>2026-09-13</current_date>\n  \
        <timezone>Asia/Shanghai</timezone>\n</environment_context>";
    let quoted_context = format!("Please explain this example:\n```xml\n{normal_chat}\n```");
    let body = json!({
        "model": "client-model",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<environment_context>\n  <cwd>/Users/mike/Personal/workspace/PORTAL2</cwd>\n  <shell>zsh</shell>\n  <current_date>2026-09-13</current_date>\n  <timezone>Asia/Shanghai</timezone>\n  <filesystem><file_system type=\"unrestricted\" /></filesystem>\n</environment_context>"
                }],
                "internal_chat_message_metadata_passthrough": {
                    "create_time": 1789293131.822,
                    "content_item_kinds": ["environments.environment_context"]
                }
            },
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": normal_chat}],
                "internal_chat_message_metadata_passthrough": {
                    "content_item_kinds": ["user.text"]
                }
            },
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": quoted_context}]
            }
        ],
        "tools": [
            {"type": "web_search"},
            {
                "type": "function",
                "name": "remember_timezone",
                "description": "Keep Asia/Shanghai unchanged"
            }
        ],
        "client_metadata": {
            "x-codex-turn-metadata": "{\"turn_started_at_unix_ms\":1789293131822}",
            "x-codex-ws-stream-request-start-ms": "1789293132000"
        }
    })
    .as_object()
    .expect("request object")
    .clone();
    let before = Utc::now().with_timezone(&New_York);

    let encoded = encode_generate_request(
        &request(body),
        "gpt-test",
        Some(&CodexRequestLocation::default()),
    )
    .expect("encode");

    let after = Utc::now().with_timezone(&New_York);
    let encoded = Value::Object(encoded.body().clone());
    let environment = encoded
        .pointer("/input/0/content/0/text")
        .and_then(Value::as_str)
        .expect("environment context text");
    let expected_dates = [
        format!(
            "{:04}-{:02}-{:02}",
            before.year(),
            before.month(),
            before.day()
        ),
        format!(
            "{:04}-{:02}-{:02}",
            after.year(),
            after.month(),
            after.day()
        ),
    ];
    assert!(
        expected_dates
            .iter()
            .any(|date| environment.contains(&format!("<current_date>{date}</current_date>")))
    );
    assert!(environment.contains("<timezone>America/New_York</timezone>"));
    assert_eq!(
        encoded.pointer("/tools/0/user_location"),
        Some(&json!({
            "type": "approximate",
            "country": "US",
            "region": "Ohio",
            "city": "Piketon",
            "timezone": "America/New_York"
        }))
    );
    assert_eq!(
        encoded.pointer("/input/1/content/0/text"),
        Some(&json!(normal_chat))
    );
    assert_eq!(
        encoded.pointer("/input/2/content/0/text"),
        Some(&json!(quoted_context))
    );
    assert_eq!(
        encoded.pointer("/input/0/internal_chat_message_metadata_passthrough/create_time"),
        Some(&json!(1789293131.822))
    );
    assert_eq!(
        encoded.pointer("/tools/1/description"),
        Some(&json!("Keep Asia/Shanghai unchanged"))
    );
    assert_eq!(
        encoded.pointer("/client_metadata"),
        Some(&json!({
            "x-codex-turn-metadata": "{\"turn_started_at_unix_ms\":1789293131822}",
            "x-codex-ws-stream-request-start-ms": "1789293132000"
        }))
    );
}

#[test]
fn encoder_should_preserve_client_store_intent() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("persist inside gateway")),
        ("store".to_owned(), json!(true)),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

    assert_eq!(encoded.body().get("store"), Some(&json!(true)));
}

#[test]
fn encoder_should_forward_the_client_prompt_cache_key() {
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("cache prefix")),
        ("prompt_cache_key".to_owned(), json!("cache-route")),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

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

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

    assert_eq!(
        encoded.client_conversation_id.as_deref(),
        Some("cache-conversation")
    );
}

#[test]
fn encoder_should_extract_websocket_context_from_client_metadata() {
    let metadata = json!({
        "session_id": "metadata-session",
        "thread_id": "metadata-thread",
        "x-codex-turn-state": "metadata-turn-state",
        "turn_id": "metadata-turn-id",
        "future_context": {"keep": true}
    });
    let request = request(Map::from_iter([
        ("model".to_owned(), json!("client-model")),
        ("input".to_owned(), json!("prompt")),
        ("client_metadata".to_owned(), metadata.clone()),
    ]));

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

    assert_eq!(
        encoded.client_session_id.as_deref(),
        Some("metadata-session")
    );
    assert_eq!(encoded.client_thread_id.as_deref(), Some("metadata-thread"));
    assert_eq!(encoded.turn_state.as_deref(), Some("metadata-turn-state"));
    assert_eq!(encoded.client_turn_id.as_deref(), Some("metadata-turn-id"));
    assert_eq!(encoded.body().get("client_metadata"), Some(&metadata));
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
            (
                "client_metadata".to_owned(),
                json!({
                    "x-codex-turn-state": "metadata-turn-state",
                    "turn_id": "metadata-turn-id"
                }),
            ),
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
        (
            "turn_id".to_owned(),
            Value::String("header-turn-id".to_owned()),
        ),
    ]));
    let request = GenerateRequest::from_protocol_payload(payload);

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

    assert_eq!(encoded.turn_state.as_deref(), Some("header-turn-state"));
    assert_eq!(
        encoded.turn_metadata.as_deref(),
        Some("header-turn-metadata")
    );
    assert_eq!(encoded.client_turn_id.as_deref(), Some("header-turn-id"));
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

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

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

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

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

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

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

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

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

    let encoded =
        encode_generate_request(&request, "gpt-test", None).expect("opaque options encode");

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

    let encoded = encode_generate_request(&request, "gpt-test", None).expect("encode");

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

    let semantics = encode_generate_request(&request, "observability", None)
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
    let metadata_encoded = encode_generate_request(&metadata_request, "gpt-test", None)
        .expect("encode metadata request");
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
    let turn_metadata_encoded = encode_generate_request(&turn_metadata_request, "gpt-test", None)
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

    let semantics = encode_generate_request(&request, "observability", None)
        .expect("transparent OpenAI request should encode")
        .semantics();

    assert_eq!(semantics.reasoning_preset, Some("ultra"));
    assert!(semantics.compact);
}
