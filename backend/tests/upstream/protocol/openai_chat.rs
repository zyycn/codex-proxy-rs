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
fn chat_completion_request_should_flatten_function_tools_for_codex_request() {
    let mut request = basic_chat_request();
    request.tools = Some(vec![
        json!({
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "Lookup docs",
                "parameters": {"type": "object"},
                "strict": true,
            },
        }),
        json!({
            "type": "web_search_preview",
            "search_context_size": "low",
        }),
    ]);

    let codex = codex_proxy_rs::proxy::openai::chat::translate_chat_to_codex(request)
        .expect("chat request should translate");

    assert_eq!(
        codex.tools,
        Some(vec![
            json!({
                "type": "function",
                "name": "lookup",
                "description": "Lookup docs",
                "parameters": {"type": "object"},
                "strict": true,
            }),
            json!({
                "type": "web_search_preview",
                "search_context_size": "low",
            }),
        ])
    );
}

#[test]
fn chat_completion_request_should_flatten_legacy_functions_for_codex_request() {
    let mut request = basic_chat_request();
    request.functions = Some(vec![json!({
        "name": "lookup",
        "description": "Lookup docs",
        "parameters": {"type": "object"},
        "strict": false,
    })]);

    let codex = codex_proxy_rs::proxy::openai::chat::translate_chat_to_codex(request)
        .expect("chat request should translate");

    assert_eq!(
        codex.tools,
        Some(vec![json!({
            "type": "function",
            "name": "lookup",
            "description": "Lookup docs",
            "parameters": {"type": "object"},
            "strict": false,
        })])
    );
}

#[test]
fn chat_completion_request_should_flatten_function_tool_choice_for_codex_request() {
    let mut request = basic_chat_request();
    request.tool_choice = Some(json!({
        "type": "function",
        "function": {"name": "lookup"},
    }));

    let codex = codex_proxy_rs::proxy::openai::chat::translate_chat_to_codex(request)
        .expect("chat request should translate");

    assert_eq!(
        codex.tool_choice,
        Some(json!({
            "type": "function",
            "name": "lookup",
        }))
    );
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
fn chat_completion_from_codex_sse_should_convert_incomplete_response() {
    let body = format!(
        "{}\n",
        include_str!("../../fixtures/responses/http_sse/chat_delta_incomplete_usage.sse")
    );

    let response = codex_proxy_rs::proxy::openai::chat::chat_completion_from_codex_sse(
        &body, "gpt-5.5", false, None,
    )
    .expect("conversion should succeed")
    .expect("terminal response");

    assert_eq!(response["choices"][0]["finish_reason"], "length");
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

#[test]
fn chat_completion_stream_translator_should_complete_incomplete_response() {
    let mut translator = codex_proxy_rs::proxy::openai::chat::ChatCompletionStreamTranslator::new(
        "gpt-5.5", false, None,
    );
    let body = format!(
        "{}\n",
        include_str!("../../fixtures/responses/http_sse/chat_delta_incomplete_usage.sse")
    );
    let output = format!(
        "{}{}",
        translator.initial_frame(),
        translator
            .push_str(&body)
            .expect("stream conversion should succeed")
    );

    assert_substrings_appear_in_order(
        &output,
        &[
            "\"delta\":{\"role\":\"assistant\"}",
            "\"delta\":{\"content\":\"hello\"}",
            "\"finish_reason\":\"length\"",
            "\"usage\":{\"completion_tokens\":3,\"prompt_tokens\":2,\"total_tokens\":5}",
            "data: [DONE]",
        ],
    );
}

#[test]
fn chat_completion_stream_translator_should_emit_reasoning_text_delta() {
    let mut translator = codex_proxy_rs::proxy::openai::chat::ChatCompletionStreamTranslator::new(
        "gpt-5.5", true, None,
    );
    let output = translator
        .push_str("event: response.reasoning_text.delta\ndata: {\"delta\":\"thinking\"}\n\n")
        .expect("stream conversion should succeed");

    assert!(output.contains("\"delta\":{\"reasoning_content\":\"thinking\"}"));
}

#[test]
fn chat_completion_stream_translator_should_emit_custom_tool_input_delta() {
    let mut translator = codex_proxy_rs::proxy::openai::chat::ChatCompletionStreamTranslator::new(
        "gpt-5.5", false, None,
    );
    let body = concat!(
        "event: response.output_item.added\n",
        "data: {\"item\":{\"type\":\"custom_tool_call\",\"id\":\"item_patch\",\"call_id\":\"call_patch\",\"name\":\"apply_patch\"}}\n\n",
        "event: response.custom_tool_call_input.delta\n",
        "data: {\"item_id\":\"item_patch\",\"delta\":\"*** Begin Patch\"}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );
    let output = format!(
        "{}{}",
        translator.initial_frame(),
        translator
            .push_str(body)
            .expect("stream conversion should succeed")
    );

    assert_substrings_appear_in_order(
        &output,
        &[
            "\"function\":{\"arguments\":\"\",\"name\":\"apply_patch\"}",
            "\"id\":\"call_patch\"",
            "\"function\":{\"arguments\":\"*** Begin Patch\"}",
            "\"finish_reason\":\"tool_calls\"",
            "data: [DONE]",
        ],
    );
}

#[test]
fn chat_completion_from_codex_sse_should_convert_custom_tool_output_item() {
    let body = concat!(
        "event: response.output_item.done\n",
        "data: {\"item\":{\"type\":\"custom_tool_call\",\"id\":\"item_patch\",\"call_id\":\"call_patch\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\"}}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );

    let response = codex_proxy_rs::proxy::openai::chat::chat_completion_from_codex_sse(
        body, "gpt-5.5", false, None,
    )
    .expect("conversion should succeed")
    .expect("completed response");

    assert_eq!(
        response["choices"][0]["message"]["tool_calls"][0],
        json!({
            "id": "call_patch",
            "type": "function",
            "function": {
                "name": "apply_patch",
                "arguments": "*** Begin Patch",
            },
        })
    );
}

#[test]
fn chat_completion_stream_translator_should_emit_tool_arguments_from_output_item_done() {
    let mut translator = codex_proxy_rs::proxy::openai::chat::ChatCompletionStreamTranslator::new(
        "gpt-5.5", false, None,
    );
    let output = translator
        .push_str(concat!(
            "event: response.output_item.done\n",
            "data: {\"item\":{\"type\":\"function_call\",\"id\":\"item_lookup\",\"call_id\":\"call_lookup\",\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\\\"rust\\\"}\"}}\n\n",
        ))
        .expect("stream conversion should succeed");

    assert_substrings_appear_in_order(
        &output,
        &[
            "\"function\":{\"arguments\":\"\",\"name\":\"lookup\"}",
            "\"id\":\"call_lookup\"",
            "\"function\":{\"arguments\":\"{\\\"q\\\":\\\"rust\\\"}\"}",
        ],
    );
}

#[test]
fn chat_completion_stream_translator_should_map_response_failed_to_openai_error() {
    let mut translator = codex_proxy_rs::proxy::openai::chat::ChatCompletionStreamTranslator::new(
        "gpt-5.5", false, None,
    );
    let body = format!(
        "{}\n\n",
        include_str!("../../fixtures/responses/http_sse/failed_quota.sse")
    );
    let error = translator
        .push_str(&body)
        .expect_err("response.failed should terminate chat stream conversion");

    let codex_proxy_rs::proxy::openai::chat::ChatStreamTranslationError::Upstream {
        message,
        error_type,
        code,
    } = error
    else {
        panic!("expected upstream stream error");
    };
    assert_eq!(message, "quota exhausted");
    assert_eq!(error_type, "insufficient_quota");
    assert_eq!(code, "insufficient_quota");
}

#[test]
fn chat_completion_stream_translator_should_map_top_level_error_chunk_to_openai_error() {
    let mut translator = codex_proxy_rs::proxy::openai::chat::ChatCompletionStreamTranslator::new(
        "gpt-5.5", false, None,
    );
    let error = translator
        .push_str(
            "event: error\n\
             data: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"usage limit reached\"}\n\n",
        )
        .expect_err("top-level error chunk should terminate chat stream conversion");

    let codex_proxy_rs::proxy::openai::chat::ChatStreamTranslationError::Upstream {
        message,
        error_type,
        code,
    } = error
    else {
        panic!("expected upstream stream error");
    };
    assert_eq!(message, "usage limit reached");
    assert_eq!(error_type, "rate_limit_error");
    assert_eq!(code, "rate_limit_exceeded");
}

fn basic_chat_request() -> codex_proxy_rs::proxy::openai::chat::ChatCompletionRequest {
    codex_proxy_rs::proxy::openai::chat::ChatCompletionRequest {
        model: "gpt-5.5".to_string(),
        stream: false,
        messages: vec![codex_proxy_rs::proxy::openai::chat::ChatMessage {
            role: "user".to_string(),
            content: Some(json!("hello")),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            function_call: None,
        }],
        reasoning_effort: None,
        service_tier: None,
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        functions: None,
        response_format: None,
        user: None,
    }
}
