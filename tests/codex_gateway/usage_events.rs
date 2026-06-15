use serde_json::json;

use codex_proxy_rs::codex::gateway::transport::{
    sse::{encode_sse_event, parse_sse_events, SseEvent, MAX_SSE_EVENT_BUFFER_BYTES},
    usage_events::{extract_sse_usage, extract_usage, TokenUsage},
};

const SSE_STANDARD_PARSE_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/sse_standard_events.json");
const SSE_PRETTY_JSON_PARSE_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/sse_pretty_json_events.json");
const NON_SSE_BODY_PARSE_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/non_sse_body_event.json");

#[test]
fn parse_sse_events_combines_multiline_data_and_metadata() {
    let input = concat!(
        ": keepalive\n",
        "id: evt_1\n",
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"hel\"}\n",
        "data: {\"delta\":\"lo\"}\n",
        "retry: 5000\n",
        "\n",
    );

    let events = parse_sse_events(input).unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id.as_deref(), Some("evt_1"));
    assert_eq!(
        events[0].event.as_deref(),
        Some("response.output_text.delta")
    );
    assert_eq!(events[0].data, "{\"delta\":\"hel\"}\n{\"delta\":\"lo\"}");
    assert_eq!(events[0].retry, Some(5000));
    assert_eq!(sse_events_json(&events), SSE_STANDARD_PARSE_GOLDEN);
}

#[test]
fn parse_sse_events_should_keep_non_prefixed_json_continuation_lines() {
    let input = concat!(
        "event: error\n",
        "data: {\n",
        "  \"error\": {\n",
        "    \"message\": \"bad upstream\"\n",
        "  }\n",
        "}\n",
        "\n",
    );

    let events = parse_sse_events(input).unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].data,
        "{\n  \"error\": {\n    \"message\": \"bad upstream\"\n  }\n}"
    );
    assert_eq!(sse_events_json(&events), SSE_PRETTY_JSON_PARSE_GOLDEN);
}

#[test]
fn parse_sse_events_should_ignore_done_sentinel_without_event_type() {
    let events = parse_sse_events("data: [DONE]\r\n\r\n").unwrap();

    assert!(events.is_empty());
}

#[test]
fn parse_sse_events_should_convert_non_sse_json_body_to_error_event() {
    let events = parse_sse_events(r#"{"error":{"message":"not an SSE stream"}}"#).unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.as_deref(), Some("error"));
    let data: serde_json::Value = serde_json::from_str(&events[0].data).unwrap();
    assert_eq!(data["error"]["code"], "non_sse_response");
    assert_eq!(data["error"]["message"], "not an SSE stream");
    assert_eq!(sse_events_json(&events), NON_SSE_BODY_PARSE_GOLDEN);
}

#[test]
fn parse_sse_events_should_reject_single_event_buffer_above_original_limit() {
    let mut input = String::from("event: response.output_text.delta\ndata: ");
    input.extend(std::iter::repeat_n('x', MAX_SSE_EVENT_BUFFER_BYTES));

    let Err(error) = parse_sse_events(&input) else {
        panic!("oversized SSE event was accepted");
    };

    assert_eq!(
        error.to_string(),
        "SSE buffer exceeded 67108864 bytes — aborting stream"
    );
}

#[test]
fn encode_sse_event_emits_event_and_data_frame() {
    let encoded = encode_sse_event("error", r#"{"error":{"code":"upstream_error"}}"#);

    assert_eq!(
        encoded,
        "event: error\ndata: {\"error\":{\"code\":\"upstream_error\"}}\n\n"
    );
}

#[test]
fn extract_usage_reads_codex_token_usage_shape() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "input_tokens_details": {
                "cached_tokens": 3
            }
        }
    });

    let usage = extract_usage(&body).unwrap();

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 17,
        }
    );
}

#[test]
fn extract_usage_reads_image_generation_tool_usage_separately() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "input_tokens_details": {
                "cached_tokens": 3
            }
        },
        "tool_usage": {
            "image_gen": {
                "input_tokens": 31,
                "output_tokens": 9
            }
        }
    });

    let usage = extract_usage(&body).unwrap();
    let serialized = serde_json::to_value(usage).unwrap();

    assert_eq!(serialized["imageInputTokens"], 31);
    assert_eq!(serialized["imageOutputTokens"], 9);
    assert_eq!(serialized["inputTokens"], 12);
    assert_eq!(serialized["outputTokens"], 5);
}

#[test]
fn extract_usage_reads_openai_token_usage_shape_and_merges() {
    let first = extract_usage(&json!({
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 4,
            "prompt_tokens_details": {
                "cached_tokens": 2
            }
        }
    }))
    .unwrap();
    let second = TokenUsage {
        input_tokens: 1,
        output_tokens: 2,
        cached_tokens: 0,
        image_input_tokens: 0,
        image_output_tokens: 0,
        total_tokens: 3,
    };

    assert_eq!(
        first.merged(second),
        TokenUsage {
            input_tokens: 9,
            output_tokens: 6,
            cached_tokens: 2,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 15,
        }
    );
}

#[test]
fn extract_sse_usage_should_use_completed_response_usage_without_merging_earlier_usage() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n\n",
    );

    let usage = extract_sse_usage(body).unwrap().unwrap();

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 3,
            output_tokens: 5,
            cached_tokens: 1,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 8,
        }
    );
}

#[test]
fn extract_sse_usage_should_read_completed_image_generation_tool_usage() {
    let body = include_str!("../fixtures/responses/http_sse/completed_image_usage.sse");

    let usage = extract_sse_usage(body).unwrap().unwrap();
    let serialized = serde_json::to_value(usage).unwrap();

    assert_eq!(serialized["imageInputTokens"], 31);
    assert_eq!(serialized["imageOutputTokens"], 9);
}

fn sse_events_json(events: &[SseEvent]) -> String {
    let value = serde_json::Value::Array(
        events
            .iter()
            .map(|event| {
                json!({
                    "event": event.event,
                    "data": event.data,
                    "id": event.id,
                    "retry": event.retry,
                })
            })
            .collect(),
    );

    format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
}
