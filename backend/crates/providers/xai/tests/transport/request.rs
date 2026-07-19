use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, ProviderOptions, ReasoningEffort,
    ReasoningRequirement, ReasoningSummary, ResponsePersistence, ToolDefinition,
};
use serde_json::{Value, json};

use provider_xai::{GrokRequestEncodeError, GrokResponsesRequest};

#[test]
fn encoder_should_project_typed_generate_semantics() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("private prompt".to_owned())],
    )
    .expect("message is valid");
    let tool = ToolDefinition::new(
        "lookup",
        Some("lookup description".to_owned()),
        json!({"type":"object"})
            .as_object()
            .cloned()
            .expect("schema is object"),
    )
    .expect("tool is valid")
    .with_strict(true);
    let request = GenerateRequest::new(vec![message])
        .expect("generate is valid")
        .with_tools(vec![tool])
        .with_max_output_tokens(256)
        .with_response_persistence(ResponsePersistence::DoNotStore)
        .with_reasoning(ReasoningRequirement {
            effort: Some(ReasoningEffort::High),
            summary: Some(ReasoningSummary::Concise),
        });

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-code-test").expect("typed request encodes");
    let body = Value::Object(encoded.body().clone());

    assert_eq!(
        (
            body.pointer("/store"),
            body.pointer("/tools/0/type"),
            body.pointer("/reasoning/effort")
        ),
        (
            Some(&json!(false)),
            Some(&json!("function")),
            Some(&json!("high"))
        )
    );
}

#[test]
fn downstream_store_intent_should_not_enable_upstream_storage() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("persist inside gateway".to_owned())],
    )
    .expect("message is valid");
    let request = GenerateRequest::new(vec![message])
        .expect("generate is valid")
        .with_response_persistence(ResponsePersistence::Store);

    let encoded =
        GrokResponsesRequest::encode(&request, "grok-code-test").expect("typed request encodes");

    assert_eq!(encoded.body().get("store"), Some(&json!(false)));
}

#[test]
fn request_debug_should_not_expose_prompt() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("private prompt".to_owned())],
    )
    .expect("message is valid");
    let request = GenerateRequest::new(vec![message]).expect("generate is valid");
    let encoded =
        GrokResponsesRequest::encode(&request, "grok-code-test").expect("typed request encodes");

    assert!(!format!("{encoded:?}").contains("private prompt"));
}

#[test]
fn encoder_should_reject_unknown_or_non_ascii_provider_options() {
    for raw_options in [
        json!({"schema_version": 1, "raw_body": {"store": true}}),
        json!({"schema_version": 1, "conversation_id": "会话"}),
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
        let request = GenerateRequest::new(vec![
            Message::new(
                MessageRole::User,
                vec![ContentPart::Text("private prompt".to_owned())],
            )
            .expect("message"),
        ])
        .expect("request")
        .with_provider_options(provider_options);

        let error = GrokResponsesRequest::encode(&request, "grok-code-test")
            .expect_err("unsafe provider option must fail");

        assert!(matches!(
            error,
            GrokRequestEncodeError::UnsupportedProviderOption
                | GrokRequestEncodeError::InvalidProviderOptions
        ));
    }
}
