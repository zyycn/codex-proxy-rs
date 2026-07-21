use gateway_core::event::{
    CompactionSummary, GatewayEvent, ProviderEvent, ReasoningDelta, TextDelta, ToolCallDelta,
};
use gateway_core::operation::{CompactConversationRequest, GenerateRequest, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use provider_xai::{
    GrokCompactionDecodeError, GrokCompactionRequest, GrokCompactionSummaryDecoder,
    GrokRequestEncodeError,
};
use serde_json::{Value, json};

const GROK_COMPACTION_REQUEST_FIXTURE: &str = include_str!("fixtures/grok_compaction_request.json");

fn compact_request(body: Value) -> CompactConversationRequest {
    let body = body.as_object().expect("request object").clone();
    let payload = ProtocolPayload::json_object("openai", body).expect("OpenAI payload");
    CompactConversationRequest::new(GenerateRequest::from_protocol_payload(Vec::new(), payload))
}

fn encode(body: Value) -> Result<GrokCompactionRequest, GrokRequestEncodeError> {
    GrokCompactionRequest::encode(
        &compact_request(body),
        "grok-4.5",
        &ClientApiKeyId::new("key_compaction").expect("client API key ID"),
    )
}

fn valid_summary(secret: &str) -> String {
    format!(
        "<summary>\nPrivate state: {secret}\n{}\n</summary>",
        "preserved implementation context ".repeat(20)
    )
}

fn decode_summary(raw: &str) -> Result<CompactionSummary, GrokCompactionDecodeError> {
    let mut decoder = GrokCompactionSummaryDecoder::new();
    decoder.observe(&ProviderEvent::canonical(GatewayEvent::TextDelta(
        TextDelta {
            content_index: 0,
            text: raw.to_owned(),
        },
    )))?;
    decoder.finish()
}

#[test]
fn encoder_should_preserve_history_order_and_append_summary_prompt() {
    let request = encode(json!({
        "model": "client-model",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "first"}]},
            {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "result"},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "last"}]}
        ],
        "stream": true
    }))
    .expect("compaction request");

    let input = request.body()["input"].as_array().expect("input array");
    assert_eq!(
        input
            .iter()
            .take(4)
            .map(|item| item["type"].as_str().expect("item type"))
            .collect::<Vec<_>>(),
        [
            "message",
            "function_call",
            "function_call_output",
            "message"
        ]
    );
    assert_eq!(input.last().expect("summary prompt")["role"], "user");
}

#[test]
fn encoder_should_preserve_tools_and_remove_non_prefix_constraints() {
    let request = encode(json!({
        "model": "client-model",
        "input": [{"type": "message", "role": "user", "content": "history"}],
        "tools": [{"type": "function", "name": "lookup", "parameters": {"type": "object"}}],
        "tool_choice": "required",
        "parallel_tool_calls": true,
        "text": {"format": {"type": "json_object"}},
        "previous_response_id": "resp_private",
        "prompt_cache_key": "cache_private",
        "service_tier": "priority",
        "max_output_tokens": 16,
        "stream": false,
        "store": true
    }))
    .expect("compaction request");

    let body = request.body();
    let forbidden = [
        "parallel_tool_calls",
        "text",
        "previous_response_id",
        "prompt_cache_key",
        "service_tier",
        "max_output_tokens",
    ];
    assert!(forbidden.into_iter().all(|field| !body.contains_key(field)));
    assert_eq!(
        body.get("tools"),
        Some(&json!([{
            "type": "function",
            "name": "lookup",
            "parameters": {"type": "object"}
        }]))
    );
    assert_eq!(body.get("tool_choice"), Some(&json!("auto")));
    assert_eq!(body.get("temperature"), Some(&json!(1.0)));
    assert_eq!(body.get("stream"), Some(&Value::Bool(true)));
    assert_eq!(body.get("store"), Some(&Value::Bool(false)));
}

#[test]
fn encoder_should_match_grok_compaction_fixture_shape() {
    let request = encode(json!({
        "model": "client-model",
        "input": [{"type": "message", "role": "user", "content": "history"}],
        "tools": [{"type": "function", "name": "lookup", "parameters": {"type": "object"}}]
    }))
    .expect("compaction request");
    let expected: Value =
        serde_json::from_str(GROK_COMPACTION_REQUEST_FIXTURE).expect("fixture JSON");
    let mut actual = Value::Object(request.body().clone());
    *actual
        .pointer_mut("/input/1/content/0/text")
        .expect("compaction prompt") = Value::String("__GROK_COMPACTION_PROMPT__".to_owned());

    assert_eq!(actual, expected);
}

#[test]
fn encoder_should_append_structured_full_replace_prompt() {
    let request = encode(json!({
        "model": "client-model",
        "input": [{"type": "message", "role": "user", "content": "history"}]
    }))
    .expect("compaction request");
    let prompt = request.body()["input"][1]["content"][0]["text"]
        .as_str()
        .expect("compaction prompt");

    assert!(prompt.starts_with("Your task is to produce a faithful, concise summary"));
    assert!(prompt.contains("1. Primary Request and Intent"));
    assert!(prompt.contains("9. Optional Next Step"));
    assert!(prompt.contains("ONLY the <summary>...</summary> block"));
    assert!(!prompt.contains("{user_context_section}"));
}

#[test]
fn encoder_should_fail_closed_when_raw_trigger_reaches_provider() {
    let error = match encode(json!({
        "model": "client-model",
        "input": [
            {"type": "message", "role": "user", "content": "history"},
            {"type": "compaction_trigger"}
        ]
    })) {
        Ok(_) => panic!("raw trigger must not cross the typed operation boundary"),
        Err(error) => error,
    };

    assert_eq!(error, GrokRequestEncodeError::InvalidRequestNormalization);
}

#[test]
fn decoder_should_return_clean_typed_summary_after_normal_terminal() {
    let summary = decode_summary(&valid_summary("checkpoint-only-secret")).expect("summary");

    assert!(summary.as_str().starts_with("Summary:\nPrivate state:"));
    assert!(!summary.as_str().contains("<summary>"));
}

#[test]
fn decoder_should_remove_nested_leading_scratchpad() {
    let raw = format!(
        "<summary>\n<analysis>private scratchpad</analysis>\nActual state\n{}\n</summary>",
        "preserved implementation context ".repeat(20)
    );
    let summary = decode_summary(&raw).expect("clean summary");

    assert!(summary.as_str().contains("Actual state"));
    assert!(!summary.as_str().contains("private scratchpad"));
}

#[test]
fn decoder_should_remove_top_level_leading_scratchpad() {
    let raw = format!(
        "<analysis>private scratchpad</analysis>\n{}",
        valid_summary("actual state")
    );
    let summary = decode_summary(&raw).expect("clean summary");

    assert!(!summary.as_str().contains("private scratchpad"));
}

#[test]
fn decoder_should_keep_summary_after_unclosed_leading_scratchpad() {
    let raw = format!(
        "<analysis>truncated private scratchpad\n{}",
        valid_summary("actual state")
    );
    let summary = decode_summary(&raw).expect("clean summary");

    assert!(summary.as_str().contains("actual state"));
}

#[test]
fn decoder_should_neutralize_live_compaction_control_tokens_in_body() {
    let raw = valid_summary(
        "Quoted <summary> and </summary>, <analysis> and </analysis>, \
         plus <summary_request> and </summary_request>.",
    );
    let summary = decode_summary(&raw).expect("clean summary");

    for token in [
        "<summary>",
        "</summary>",
        "<analysis>",
        "</analysis>",
        "<summary_request>",
        "</summary_request>",
    ] {
        assert!(!summary.as_str().contains(token), "live token {token}");
    }
}

#[test]
fn decoder_should_preserve_unclosed_summary_body_without_live_token() {
    let raw = format!(
        "<summary>\nActual state\n{}",
        "preserved implementation context ".repeat(20)
    );
    let summary = decode_summary(&raw).expect("clean summary");

    assert!(summary.as_str().contains("Actual state"));
    assert!(!summary.as_str().contains("<summary>"));
}

#[test]
fn decoder_should_preserve_numbered_sections_that_quote_analysis_close() {
    let raw = format!(
        "<summary>\n1. Request: keep content\n6. Quote: </analysis>\n{}\n</summary>",
        "preserved implementation context ".repeat(20)
    );
    let summary = decode_summary(&raw).expect("clean summary");

    assert!(summary.as_str().contains("1. Request: keep content"));
    assert!(summary.as_str().contains("6. Quote:"));
}

#[test]
fn decoder_should_reject_degenerate_summary() {
    let error = decode_summary("<summary>too short</summary>").expect_err("short summary");

    assert_eq!(error, GrokCompactionDecodeError::Degenerate);
}

#[test]
fn decoder_should_reject_empty_cleaned_summary_as_degenerate() {
    let error = decode_summary("<analysis>draft only</analysis>").expect_err("empty summary");

    assert_eq!(error, GrokCompactionDecodeError::Degenerate);
}

#[test]
fn decoder_should_ignore_reasoning_delta() {
    let mut decoder = GrokCompactionSummaryDecoder::new();
    decoder
        .observe(&ProviderEvent::canonical(GatewayEvent::ReasoningDelta(
            ReasoningDelta {
                content_index: 0,
                text: "private reasoning must not become summary".to_owned(),
            },
        )))
        .expect("reasoning is ignored");
    decoder
        .observe(&ProviderEvent::canonical(GatewayEvent::TextDelta(
            TextDelta {
                content_index: 1,
                text: valid_summary("typed summary"),
            },
        )))
        .expect("summary text");
    let summary = decoder.finish().expect("summary");

    assert!(!summary.as_str().contains("private reasoning"));
}

#[test]
fn decoder_should_ignore_tool_call_when_summary_text_is_present() {
    let mut decoder = GrokCompactionSummaryDecoder::new();
    decoder
        .observe(&ProviderEvent::canonical(GatewayEvent::ToolCallDelta(
            ToolCallDelta {
                content_index: 0,
                call_id: "call_1".to_owned(),
                name: Some("lookup".to_owned()),
                arguments_delta: "{}".to_owned(),
            },
        )))
        .expect("tool call is ignored");
    decoder
        .observe(&ProviderEvent::canonical(GatewayEvent::TextDelta(
            TextDelta {
                content_index: 1,
                text: valid_summary("typed summary"),
            },
        )))
        .expect("summary text");
    assert!(decoder.finish().is_ok());
}

#[test]
fn decoder_should_accept_a_valid_summary_without_interpreting_terminal_state() {
    let mut decoder = GrokCompactionSummaryDecoder::new();
    decoder
        .observe(&ProviderEvent::canonical(GatewayEvent::TextDelta(
            TextDelta {
                content_index: 0,
                text: valid_summary("truncated summary"),
            },
        )))
        .expect("summary text");
    assert!(decoder.finish().is_ok());
}

#[test]
fn debug_output_should_not_contain_private_conversation_or_summary() {
    let request = encode(json!({
        "model": "client-model",
        "input": [{"type": "message", "role": "user", "content": "request-private-secret"}]
    }))
    .expect("compaction request");
    let mut decoder = GrokCompactionSummaryDecoder::new();
    decoder
        .observe(&ProviderEvent::canonical(GatewayEvent::TextDelta(
            TextDelta {
                content_index: 0,
                text: valid_summary("summary-private-secret"),
            },
        )))
        .expect("summary delta");
    let debug = format!("{request:?} {decoder:?}");

    assert!(!debug.contains("request-private-secret"));
    assert!(!debug.contains("summary-private-secret"));
}
