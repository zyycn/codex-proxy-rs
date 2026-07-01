use super::*;

#[test]
fn chat_completion_request_should_translate_to_codex_request() {
    let request = codex_proxy_rs::proxy::openai::chat::ChatCompletionRequest {
        model: "gpt-5.5".to_string(),
        stream: true,
        messages: vec![
            codex_proxy_rs::proxy::openai::chat::ChatMessage {
                role: "system".to_string(),
                content: Some(json!("be brief")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                function_call: None,
            },
            codex_proxy_rs::proxy::openai::chat::ChatMessage {
                role: "user".to_string(),
                content: Some(json!("hello")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                function_call: None,
            },
        ],
        reasoning_effort: Some("medium".to_string()),
        service_tier: Some("auto".to_string()),
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        functions: None,
        response_format: None,
        user: Some(" client-123 ".to_string()),
    };

    let codex = codex_proxy_rs::proxy::openai::chat::translate_chat_to_codex(request)
        .expect("chat request should translate");

    assert_eq!(codex.model, "gpt-5.5");
    assert!(codex.use_websocket);
    assert!(!codex.force_http_sse);
    assert_eq!(codex.prompt_cache_key.as_deref(), Some("client-123"));
    assert_eq!(codex.client_conversation_id.as_deref(), Some("client-123"));
}

#[test]
fn sse_parser_should_combine_multiline_data_and_metadata() {
    let events = codex_proxy_rs::upstream::protocol::sse::parse_sse_events(include_str!(
        "../../fixtures/sse/multiline_data_with_metadata.sse"
    ))
    .expect("sse should parse");

    assert_eq!(events[0].data, "hello\nworld");
}

#[test]
fn sse_body_has_done_should_detect_done_frame() {
    assert!(sse_body_has_done(DONE_SSE_FRAME));
    assert!(sse_body_has_done(include_str!(
        "../../fixtures/responses/http_sse/completed_empty_done_crlf.sse"
    )));
    assert!(!sse_body_has_done(include_str!(
        "../../fixtures/responses/http_sse/completed_empty.sse"
    )));
}

#[test]
fn chat_completion_from_codex_sse_should_convert_completed_response() {
    let body = include_str!("../../fixtures/responses/http_sse/chat_delta_completed_usage.sse");

    let response = codex_proxy_rs::proxy::openai::chat::chat_completion_from_codex_sse(
        body, "gpt-5.5", false, None,
    )
    .expect("conversion should succeed")
    .expect("completed response");

    assert_eq!(response["choices"][0]["message"]["content"], "hello");
}

#[test]
fn chat_completion_stream_translator_should_emit_openai_chunks() {
    let mut translator = codex_proxy_rs::proxy::openai::chat::ChatCompletionStreamTranslator::new(
        "gpt-5.5", false, None,
    );
    let body =
        include_str!("../../fixtures/responses/http_sse/chat_delta_completed_usage_with_id.sse");
    let split_at = body.find("llo").expect("fixture should split inside hello");
    let (first, rest) = body.split_at(split_at);
    let pending_output = translator
        .push_str(first)
        .expect("partial event should be buffered");
    assert!(pending_output.is_empty());
    let output = format!(
        "{}{}",
        translator.initial_frame(),
        translator
            .push_str(rest)
            .expect("stream conversion should succeed")
    );

    assert_substrings_appear_in_order(
        &output,
        &[
            "\"delta\":{\"role\":\"assistant\"}",
            "\"delta\":{\"content\":\"hello\"}",
            "\"finish_reason\":\"stop\"",
            "\"usage\":{\"completion_tokens\":3,\"prompt_tokens\":2,\"total_tokens\":5}",
            "data: [DONE]",
        ],
    );
}
