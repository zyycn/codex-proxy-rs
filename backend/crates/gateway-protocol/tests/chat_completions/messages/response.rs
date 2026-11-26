//! Messages JSON 的 Chat 投影保留工具身份和原生用量

use gateway_protocol::chat_completions::messages::{
    project_response, project_response_with_request,
};
use serde_json::json;

#[test]
fn projects_text_reasoning_custom_tools_citations_and_cache_usage() {
    let mut request = super::super::request();
    request.insert(
        "tools".into(),
        json!([{"type":"custom","custom":{"name":"shell"}}]),
    );
    let native = json!({"type":"message","role":"assistant","content":[{"type":"thinking","thinking":"reason","signature":"private"},{"type":"text","text":"answer","citations":[{"type":"web_search_result_location","url":"https://example.com","title":"example"}]},{"type":"tool_use","id":"a","name":"shell","input":{"input":"pwd"}}],"stop_reason":"tool_use","usage":{"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":7,"output_tokens":11}});
    let result =
        project_response_with_request(&native, &super::super::identity(), &request).unwrap();
    let message = &result["choices"][0]["message"];
    assert_eq!(message["content"], "answer");
    assert_eq!(message["reasoning_content"], "reason");
    assert_eq!(
        message["tool_calls"][0],
        json!({"id":"a","type":"custom","custom":{"name":"shell","input":"pwd"}})
    );
    assert_eq!(message["annotations"][0]["url_citation"]["end_index"], 6);
    assert_eq!(result["usage"]["prompt_tokens"], 15);
    assert_eq!(result["usage"]["total_tokens"], 26);
    assert_eq!(
        result["usage"]["prompt_tokens_details"]["cache_write_tokens"],
        7
    );
    assert!(!result.to_string().contains("private"));
}

#[test]
fn missing_usage_is_unknown_and_stop_reasons_are_explicit() {
    for (native, chat) in [
        ("end_turn", "stop"),
        ("tool_use", "tool_calls"),
        ("max_tokens", "length"),
        ("refusal", "content_filter"),
    ] {
        let result = project_response(
            &json!({"type":"message","role":"assistant","content":[],"stop_reason":native}),
            &super::super::identity(),
        )
        .unwrap();
        assert_eq!(result["choices"][0]["finish_reason"], chat);
        assert!(result.get("usage").is_none());
    }
    assert!(
        project_response(
            &json!({"type":"message","role":"assistant","content":[]}),
            &super::super::identity()
        )
        .is_err()
    );
}
