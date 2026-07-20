use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, ProtocolPayload, ProviderOptions,
};
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
            {"type": "future_hosted_tool", "future": [1, 2, 3]}
        ],
        "tool_choice": "auto",
        "future_official_field": {"nested": [true, 7]},
        "stream": false,
        "store": true
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-routed").expect("raw request");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(body.pointer("/model"), Some(&json!("grok-routed")));
    assert_eq!(body.pointer("/stream"), Some(&json!(true)));
    assert_eq!(body.pointer("/store"), Some(&json!(false)));
    assert_eq!(
        body.pointer("/input/0/content/1/future_image_field"),
        Some(&json!({"keep": true}))
    );
    assert_eq!(
        body.pointer("/tools/2"),
        Some(&json!({"type": "future_hosted_tool", "future": [1, 2, 3]}))
    );
    assert_eq!(
        body.pointer("/future_official_field"),
        Some(&json!({"nested": [true, 7]}))
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

    let encoded = GrokResponsesRequest::encode(&request, "grok-routed").expect("sanitized request");
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
        GrokResponsesRequest::encode(&request, "grok-routed")
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
    let encoded = GrokResponsesRequest::encode(&request, "grok-routed").expect("raw request");
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
            GrokResponsesRequest::encode(&request, "grok-routed"),
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
            ]),
        )
        .expect("provider options");
    let request = raw_request(json!({"model": "client", "input": "hello"}))
        .with_provider_options(provider_options);

    assert!(GrokResponsesRequest::encode(&request, "grok-routed").is_ok());
}
