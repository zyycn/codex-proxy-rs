//! GenAI 结果的工具、用量、Unicode 引用和安全终态

use gateway_protocol::chat_completions::genai::{project_response, project_response_with_request};
use serde_json::json;

#[test]
fn projects_custom_calls_reasoning_and_native_usage_directly() {
    let mut request = super::super::request();
    request.insert(
        "tools".into(),
        json!([{"type":"custom","custom":{"name":"shell"}}]),
    );
    let native = json!({"candidates":[{"content":{"parts":[{"thought":true,"text":"reason","thoughtSignature":"private"},{"text":"answer"},{"functionCall":{"id":"a","name":"shell","args":{"input":"pwd"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"toolUsePromptTokenCount":2,"candidatesTokenCount":5,"thoughtsTokenCount":7,"cachedContentTokenCount":1,"totalTokenCount":17}});
    let result =
        project_response_with_request(&native, &super::super::identity(), &request).unwrap();
    assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        result["choices"][0]["message"]["tool_calls"][0],
        json!({"id":"a","type":"custom","custom":{"name":"shell","input":"pwd"}})
    );
    assert_eq!(
        result["choices"][0]["message"]["reasoning_content"],
        "reason"
    );
    assert_eq!(result["usage"]["prompt_tokens"], 5);
    assert_eq!(result["usage"]["completion_tokens"], 12);
    assert_eq!(
        result["usage"]["completion_tokens_details"]["reasoning_tokens"],
        7
    );
    assert!(!result.to_string().contains("private"));
}

#[test]
fn grounding_converts_byte_offsets_to_chat_character_offsets() {
    let native = json!({"candidates":[{"content":{"parts":[{"text":"你好🌍"}]},"finishReason":"STOP","groundingMetadata":{"groundingChunks":[{"web":{"uri":"https://example.com","title":"source"}}],"groundingSupports":[{"segment":{"startIndex":3,"endIndex":10,"text":"好🌍"},"groundingChunkIndices":[0]}]}}]});
    let result = project_response(&native, &super::super::identity()).unwrap();
    assert_eq!(
        result["choices"][0]["message"]["annotations"][0]["url_citation"],
        json!({"url":"https://example.com","title":"source","start_index":1,"end_index":3})
    );
    assert!(result.get("usage").is_none());
}

#[test]
fn blocked_prompts_and_errors_are_not_normal_successes() {
    let result = project_response(
        &json!({"promptFeedback":{"blockReason":"SAFETY"}}),
        &super::super::identity(),
    )
    .unwrap();
    assert_eq!(result["choices"][0]["finish_reason"], "content_filter");
    let error = json!({"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"busy"}});
    assert_eq!(
        project_response(&error, &super::super::identity()).unwrap(),
        error
    );
    assert!(
        project_response(
            &json!({"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}),
            &super::super::identity()
        )
        .is_err()
    );
}
