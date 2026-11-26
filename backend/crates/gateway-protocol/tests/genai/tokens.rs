//! 原生完整计数请求的优先级与字段保留

use gateway_protocol::genai::count_tokens_request;
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
