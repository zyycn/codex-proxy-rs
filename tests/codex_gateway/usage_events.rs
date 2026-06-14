use serde_json::json;

use codex_proxy_rs::codex::gateway::transport::{
    sse::{encode_sse_event, parse_sse_events},
    usage_events::{extract_usage, TokenUsage},
};

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
}

#[test]
fn parse_sse_events_accepts_done_sentinel_without_event_type() {
    let events = parse_sse_events("data: [DONE]\r\n\r\n").unwrap();

    assert_eq!(events[0].data, "[DONE]");
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
            total_tokens: 17,
        }
    );
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
        total_tokens: 3,
    };

    assert_eq!(
        first.merged(second),
        TokenUsage {
            input_tokens: 9,
            output_tokens: 6,
            cached_tokens: 2,
            total_tokens: 15,
        }
    );
}
