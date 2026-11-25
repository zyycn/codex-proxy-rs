//! 原生完整请求优先级和 Messages 计数投影合同

use gateway_protocol::genai::{count_tokens_request, count_tokens_to_messages};
use serde_json::json;

#[test]
fn full_request_wins_over_outer_contents_and_keeps_tools() {
    let full = json!({"model":"models/gemini","contents":[],"systemInstruction":{"parts":[{"text":"identity"}]},"tools":[{"functionDeclarations":[]} ]});
    for key in ["generateContentRequest", "generate_content_request"] {
        let body = json!({key:full,"contents":[{"parts":[{"text":"ignored"}]}]});
        assert_eq!(
            count_tokens_request(body.as_object().unwrap()).unwrap(),
            full.as_object().unwrap()
        );
    }
    assert!(
        count_tokens_request(json!({"generateContentRequest":null}).as_object().unwrap()).is_err()
    );
}

#[test]
fn messages_count_never_estimates_or_invents_zero() {
    assert_eq!(
        count_tokens_to_messages(&json!({"totalTokens":0})).unwrap(),
        json!({"input_tokens":0})
    );
    assert_eq!(
        count_tokens_to_messages(&json!({"totalTokens":77,"future":true})).unwrap(),
        json!({"input_tokens":77})
    );
    for invalid in [
        json!({}),
        json!({"totalTokens":-1}),
        json!({"totalTokens":"77"}),
    ] {
        assert!(count_tokens_to_messages(&invalid).is_err());
    }
}
