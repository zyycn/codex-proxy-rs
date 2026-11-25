//! 验证生成响应、多候选 SSE 以及计量尾帧的真实协议语义

use gateway_protocol::{
    genai::{GenerateContentObservation, GenerationFinish, has_finish_signal, primary_text_parts},
    openai::sse::SseEventDecoder,
};
use serde_json::{Value, json};

#[test]
fn json_generation_counts_cached_and_thought_tokens_once() {
    let response = json!({
        "candidates": [{"finishReason": "MAX_TOKENS"}],
        "usageMetadata": {
            "promptTokenCount": 100, "cachedContentTokenCount": 40, "toolUsePromptTokenCount": 7,
            "thoughtsTokenCount": 9, "candidatesTokenCount": 11, "totalTokenCount": 127,
            "promptTokensDetails": [{"modality": "IMAGE", "tokenCount": 8}],
            "toolUsePromptTokensDetails": [{"modality": "IMAGE", "tokenCount": 5}]
        }
    });
    let mut observation = GenerateContentObservation::default();
    let usage = observation.observe(&response).expect("响应携带用量");
    assert_eq!(
        (
            usage.input_tokens(),
            usage.cached,
            usage.image_input_tokens()
        ),
        (Some(107), Some(40), Some(13))
    );
    assert_eq!(
        (usage.output_tokens(), usage.thoughts, usage.total),
        (Some(20), Some(9), Some(127))
    );
    assert_eq!(
        observation.finish(),
        Some(GenerationFinish::Candidate {
            reason: "MAX_TOKENS",
            tool_call: false
        })
    );
}

#[test]
fn clean_stream_keeps_tool_signature_and_usage_after_both_candidates_finish() {
    let frames: &[&[u8]] = &[
        b":heartbeat\r\n\r\n",
        br#"data: { "candidates": [{"content":{"parts":[{"functionCall":{"name":"lookup","args":{}},"thoughtSignature":"AA=="}]}},{"index":1,"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"thoughtsTokenCount":3} }

"#,
        b"data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n",
        b"data: {\"candidates\":[{\"index\":1,\"finishReason\":\"MAX_TOKENS\"}]}\n\n",
        b"data: {\"usageMetadata\":{\"candidatesTokenCount\":7,\"totalTokenCount\":10}}\n\n",
    ];
    let bytes = frames.concat();
    let mut decoder = SseEventDecoder::default();
    let mut observation = GenerateContentObservation::default();
    let mut delivered = Vec::new();
    let mut last_total = None;
    for frame in bytes.chunks(7).flat_map(|bytes| decoder.push_frames(bytes)) {
        delivered.extend_from_slice(frame.raw());
        for event in frame.events() {
            let value: Value = serde_json::from_str(&event.data).expect("原生 SSE JSON");
            if let Some(usage) = observation.observe(&value) {
                last_total = Some((usage.output_tokens(), usage.total));
            }
            if value
                .pointer("/candidates/0/finishReason")
                .and_then(Value::as_str)
                == Some("STOP")
            {
                assert_eq!(observation.finish(), None, "第二个候选尚未结束");
            }
        }
    }
    assert!(decoder.finish_frames().is_empty());
    assert_eq!(delivered, bytes);
    assert_eq!(last_total, Some((Some(10), Some(10))));
    assert_eq!(
        observation.finish(),
        Some(GenerationFinish::Candidate {
            reason: "STOP",
            tool_call: true
        })
    );
}

#[test]
fn incomplete_candidates_and_blocked_prompts_have_different_outcomes() {
    let mut incomplete = GenerateContentObservation::default();
    incomplete.observe(&json!({"candidates":[{"content":{"parts":[{"text":"partial"}]},"finishReason":"FINISH_REASON_UNSPECIFIED"},{"index":1,"finishReason":"STOP"}]}));
    assert_eq!(incomplete.finish(), None);
    let mut blocked = GenerateContentObservation::default();
    let response = json!({"promptFeedback":{"blockReason":"SAFETY"}});
    blocked.observe(&response);
    assert!(has_finish_signal(&response));
    assert_eq!(blocked.finish(), Some(GenerationFinish::Blocked));
    assert!(!has_finish_signal(
        &json!({"usageMetadata":{"totalTokenCount":2}})
    ));
    assert!(has_finish_signal(
        &json!({"error":{"code":429,"message":"Throttled."}})
    ));
}

#[test]
fn probe_shows_visible_primary_text_without_leaking_thoughts_or_tool_payloads() {
    let response = json!({"candidates":[
        {"content":{"parts":[{"thought":true,"text":"private"},{"text":"P"},{"functionCall":{"args":{"text":"private"}},"thoughtSignature":"AA=="},{"text":"ONG"}]}},
        {"index":1,"content":{"parts":[{"text":"alternative"}]}}
    ]});
    assert_eq!(primary_text_parts(&response).collect::<String>(), "PONG");
}
