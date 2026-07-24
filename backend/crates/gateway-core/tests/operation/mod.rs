use serde_json::{Map, Value};

use gateway_core::error::OperationError;
use gateway_core::operation::{
    CompactConversationRequest, ContentPart, ContinuationMode, EmbedRequest, Feature,
    GenerateRequest, ImageRequest, ImageSource, JsonSchemaFormat, Message, MessageRole, Operation,
    OperationKind, OutputFormat, ProviderOptions, ReasoningEffort, ReasoningRequirement,
    ReasoningSummary, RerankRequest, ResponsePersistence, RetrySafety, SpeechRequest,
    ToolDefinition,
};

fn text_message(role: MessageRole, text: &str) -> Message {
    Message::new(role, vec![ContentPart::Text(text.to_owned())]).expect("message is valid")
}

#[test]
fn capability_requirements_should_include_vision_for_image_input() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Image(ImageSource::Url(
            "https://example.invalid/image.png".to_owned(),
        ))],
    )
    .expect("test message is valid");
    let request = GenerateRequest::new(vec![message]).expect("test request is valid");
    let operation = Operation::Generate(request);

    assert!(
        operation
            .capability_requirements()
            .features()
            .contains(&Feature::Vision)
    );
}

#[test]
fn capability_requirements_should_include_json_schema() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("input".to_owned())],
    )
    .expect("test message is valid");
    let format = JsonSchemaFormat::new("result", None, serde_json::Map::new(), true)
        .expect("test schema format is valid");
    let request = GenerateRequest::new(vec![message])
        .expect("test request is valid")
        .with_output_format(OutputFormat::JsonSchema(format));
    let operation = Operation::Generate(request);

    assert!(
        operation
            .capability_requirements()
            .features()
            .contains(&Feature::JsonSchema)
    );
}

#[test]
fn operation_debug_should_not_include_prompt_text() {
    let secret_prompt = "private prompt body";
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text(secret_prompt.to_owned())],
    )
    .expect("test message is valid");
    let operation =
        Operation::Generate(GenerateRequest::new(vec![message]).expect("test request is valid"));

    assert!(!format!("{operation:?}").contains(secret_prompt));
}

#[test]
fn response_persistence_should_be_explicit_in_generate_request() {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("input".to_owned())],
    )
    .expect("test message is valid");
    let request = GenerateRequest::new(vec![message])
        .expect("test request is valid")
        .with_response_persistence(ResponsePersistence::DoNotStore);

    assert_eq!(
        request.response_persistence(),
        ResponsePersistence::DoNotStore
    );
}

#[test]
fn prompt_cache_key_should_be_explicit_and_redacted_in_generate_request() {
    let request = GenerateRequest::new(vec![text_message(MessageRole::User, "input")])
        .expect("test request is valid")
        .with_prompt_cache_key("private-cache-route");

    assert_eq!(request.prompt_cache_key(), Some("private-cache-route"));
    assert!(!format!("{request:?}").contains("private-cache-route"));
}

#[test]
fn message_should_reject_empty_content() {
    let error = Message::new(MessageRole::User, Vec::new()).expect_err("content is required");

    assert_eq!(error, OperationError::EmptyField { field: "content" });
}

#[test]
fn generate_request_should_reject_empty_messages() {
    let error = GenerateRequest::new(Vec::new()).expect_err("messages are required");

    assert_eq!(error, OperationError::EmptyField { field: "messages" });
}

#[test]
fn embed_request_should_reject_empty_input() {
    let error = EmbedRequest::new(Vec::new()).expect_err("input is required");

    assert_eq!(error, OperationError::EmptyField { field: "input" });
}

#[test]
fn embed_request_should_reject_empty_item() {
    let error = EmbedRequest::new(vec!["valid".to_owned(), String::new()])
        .expect_err("every embedding input is required");

    assert_eq!(error, OperationError::EmptyField { field: "input" });
}

#[test]
fn rerank_request_should_reject_empty_query() {
    let error = RerankRequest::new("", vec!["document".to_owned()]).expect_err("query is required");

    assert_eq!(error, OperationError::EmptyField { field: "query" });
}

#[test]
fn rerank_request_should_reject_empty_documents() {
    let error = RerankRequest::new("query", Vec::new()).expect_err("documents are required");

    assert_eq!(error, OperationError::EmptyField { field: "documents" });
}

#[test]
fn rerank_request_should_reject_empty_document_item() {
    let error =
        RerankRequest::new("query", vec![String::new()]).expect_err("every document is required");

    assert_eq!(error, OperationError::EmptyField { field: "documents" });
}

#[test]
fn image_request_should_reject_empty_prompt() {
    let error = ImageRequest::new("").expect_err("prompt is required");

    assert_eq!(error, OperationError::EmptyField { field: "prompt" });
}

#[test]
fn speech_request_should_reject_empty_input() {
    let error = SpeechRequest::new("", "alloy").expect_err("speech input is required");

    assert_eq!(error, OperationError::EmptyField { field: "input" });
}

#[test]
fn speech_request_should_reject_empty_voice() {
    let error = SpeechRequest::new("hello", "").expect_err("voice is required");

    assert_eq!(error, OperationError::EmptyField { field: "voice" });
}

#[test]
fn tool_definition_should_reject_empty_name() {
    let error = ToolDefinition::new("", None, Map::new()).expect_err("tool name is required");

    assert_eq!(error, OperationError::EmptyField { field: "tool.name" });
}

#[test]
fn json_schema_format_should_reject_empty_name() {
    let error =
        JsonSchemaFormat::new("", None, Map::new(), true).expect_err("schema name is required");

    assert_eq!(
        error,
        OperationError::EmptyField {
            field: "text.format.name"
        }
    );
}

#[test]
fn provider_options_should_reject_duplicate_provider() {
    let mut options = ProviderOptions::new();
    options
        .insert("openai", Map::new())
        .expect("first provider options are valid");

    let error = options
        .insert("openai", Map::new())
        .expect_err("provider options must be unique");

    assert_eq!(
        error,
        OperationError::DuplicateProviderOptions {
            provider: "openai".to_owned()
        }
    );
}

#[test]
fn provider_options_should_reject_invalid_provider_name() {
    let mut options = ProviderOptions::new();

    let error = options
        .insert("bad\nprovider", Map::new())
        .expect_err("control characters are invalid");

    assert_eq!(
        error,
        OperationError::EmptyField {
            field: "provider_options provider"
        }
    );
}

#[test]
fn provider_options_debug_should_redact_values() {
    let secret = "provider-private-value";
    let mut codex = Map::new();
    codex.insert("opaque".to_owned(), Value::from(secret));
    let mut options = ProviderOptions::new();
    options
        .insert("openai", codex)
        .expect("provider options are valid");

    let debug = format!("{options:?}");

    assert!(debug.contains("openai"));
    assert!(!debug.contains(secret));
}

#[test]
fn provider_options_should_iterate_in_stable_provider_order() {
    let mut options = ProviderOptions::new();
    options
        .insert("xai", Map::new())
        .expect("xAI options are valid");
    options
        .insert("openai", Map::new())
        .expect("Codex options are valid");

    assert_eq!(
        options.providers().collect::<Vec<_>>(),
        vec!["openai", "xai"]
    );
}

#[test]
fn client_supplied_history_should_preserve_message_order() {
    let request = GenerateRequest::new(vec![
        text_message(MessageRole::Assistant, "previous"),
        text_message(MessageRole::User, "next"),
    ])
    .expect("client history is valid");

    assert_eq!(request.messages()[0].role(), MessageRole::Assistant);
}

#[test]
fn capability_requirements_should_include_reasoning() {
    let request = GenerateRequest::new(vec![text_message(MessageRole::User, "solve")])
        .expect("request is valid")
        .with_reasoning(ReasoningRequirement {
            effort: Some(ReasoningEffort::High),
            summary: Some(ReasoningSummary::Concise),
        });

    let requirements = Operation::Generate(request).capability_requirements();

    assert!(requirements.features().contains(&Feature::Reasoning));
}

#[test]
fn capability_requirements_should_distinguish_native_continuation() {
    let request = GenerateRequest::new(vec![text_message(MessageRole::User, "continue")])
        .expect("request is valid")
        .with_continuation(ContinuationMode::Native);

    let requirements = Operation::Generate(request).capability_requirements();

    assert!(
        requirements
            .features()
            .contains(&Feature::NativeContinuation)
    );
}

#[test]
fn capability_requirements_should_preserve_output_bound() {
    let request = GenerateRequest::new(vec![text_message(MessageRole::User, "bounded")])
        .expect("request is valid")
        .with_max_output_tokens(1_024);

    let requirements = Operation::Generate(request).capability_requirements();

    assert_eq!(requirements.requested_output_tokens(), Some(1_024));
}

#[test]
fn embed_and_rerank_operations_should_be_idempotent() {
    let embed = Operation::Embed(
        EmbedRequest::new(vec!["input".to_owned()]).expect("embedding request is valid"),
    );
    let rerank = Operation::Rerank(
        RerankRequest::new("query", vec!["document".to_owned()]).expect("rerank request is valid"),
    );

    assert_eq!(embed.retry_safety(), RetrySafety::Idempotent);
    assert_eq!(rerank.retry_safety(), RetrySafety::Idempotent);
}

#[test]
fn generate_and_compact_conversation_should_be_non_idempotent() {
    let generate = GenerateRequest::new(vec![text_message(MessageRole::User, "history")])
        .expect("compaction history is valid");
    assert_eq!(
        Operation::Generate(generate.clone()).retry_safety(),
        RetrySafety::NonIdempotent
    );
    let compact = Operation::CompactConversation(CompactConversationRequest::new(generate));

    assert_eq!(compact.retry_safety(), RetrySafety::NonIdempotent);
}

#[test]
fn operation_kind_should_match_each_payload() {
    let embed = Operation::Embed(
        EmbedRequest::new(vec!["input".to_owned()]).expect("embedding request is valid"),
    );
    let rerank = Operation::Rerank(
        RerankRequest::new("query", vec!["document".to_owned()]).expect("rerank request is valid"),
    );
    let image = Operation::GenerateImage(ImageRequest::new("draw").expect("image request"));
    let speech = Operation::Speech(SpeechRequest::new("hello", "alloy").expect("speech request"));

    assert_eq!(embed.kind(), OperationKind::Embed);
    assert_eq!(rerank.kind(), OperationKind::Rerank);
    assert_eq!(image.kind(), OperationKind::GenerateImage);
    assert_eq!(speech.kind(), OperationKind::Speech);
}

#[test]
fn content_debug_should_redact_text_and_tool_payloads() {
    let text_secret = "private-text";
    let tool_secret = "private-tool-output";
    let text = ContentPart::Text(text_secret.to_owned());
    let tool = ContentPart::ToolResult {
        call_id: "call_1".to_owned(),
        output: tool_secret.to_owned(),
    };

    assert!(!format!("{text:?}").contains(text_secret));
    assert!(!format!("{tool:?}").contains(tool_secret));
}
