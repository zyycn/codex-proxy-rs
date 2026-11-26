//! GenAI 流只在终态候选后的 EOF 交付结束和累计用量

use gateway_protocol::chat_completions::{ChatAccumulator, ChatError, genai::ChatStreamEncoder};
use serde_json::json;

#[test]
fn usage_after_finish_reason_is_included_once_at_eof() {
    let mut encoder = ChatStreamEncoder::new(super::super::identity(), true);
    let mut accumulator = ChatAccumulator::default();
    for event in [
        json!({"candidates":[{"content":{"parts":[{"text":"hello "}]}}]}),
        json!({"candidates":[{"content":{"parts":[{"text":"world"}]},"finishReason":"STOP"}]}),
        json!({"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":5,"totalTokenCount":8}}),
    ] {
        let chunks = encoder.push_event(None, &event).unwrap();
        assert!(
            chunks
                .iter()
                .all(|v| v["choices"][0]["finish_reason"].is_null())
        );
        for chunk in chunks {
            accumulator.push(&chunk).unwrap();
        }
    }
    for chunk in encoder.finish().unwrap() {
        accumulator.push(&chunk).unwrap();
    }
    assert!(encoder.finish().unwrap().is_empty());
    let result = accumulator.finish().unwrap();
    assert_eq!(result["choices"][0]["message"]["content"], "hello world");
    assert_eq!(result["usage"]["total_tokens"], 8);
}

#[test]
fn malformed_candidates_and_content_after_finished_are_rejected() {
    let mut encoder = ChatStreamEncoder::new(super::super::identity(), false);
    assert_eq!(encoder.finish().unwrap_err(), ChatError::IncompleteStream);
    assert!(
        encoder
            .push_event(
                None,
                &json!({"candidates":[{"index":1,"finishReason":"STOP"}]})
            )
            .is_err()
    );
    let mut encoder = ChatStreamEncoder::new(super::super::identity(), false);
    encoder
        .push_event(None, &json!({"candidates":[{"finishReason":"STOP"}]}))
        .unwrap();
    assert!(
        encoder
            .push_event(
                None,
                &json!({"candidates":[{"content":{"parts":[{"text":"late"}]}}]})
            )
            .is_err()
    );
}

#[test]
fn required_search_needs_upstream_grounding_evidence() {
    let mut request = super::super::request();
    request.insert("web_search_options".into(), json!({}));
    let mut encoder = ChatStreamEncoder::with_request(super::super::identity(), false, &request);
    encoder.push_event(None,&json!({"candidates":[{"content":{"parts":[{"text":"answer"}]},"finishReason":"STOP"}]})).unwrap();
    assert_eq!(encoder.finish().unwrap_err(), ChatError::InvalidResponse);
}
