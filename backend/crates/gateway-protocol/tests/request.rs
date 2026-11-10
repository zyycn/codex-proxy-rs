//! 验证不同协议的共享请求事实

use gateway_protocol::request::request_facts;
use serde_json::json;

#[test]
fn messages_facts_include_images_inside_tool_results() {
    let body = json!({"max_tokens":0,"messages":[{"role":"user","content":[{"type":"tool_result","content":[{"type":"image","source":{"type":"url","url":"https://example.com/image.png"}}]}]}],"tools":[{"name":"search"}],"thinking":{"type":"adaptive"},"output_config":{"format":{"type":"json_schema","schema":{"type":"object"}}}});
    let facts = request_facts("anthropic", body.as_object().expect("请求正文"));
    assert_eq!(facts.max_output_tokens, Some(0));
    assert!(facts.vision && facts.tools && facts.reasoning && facts.json_schema);
    assert!(!facts.native_continuation);
}

#[test]
fn responses_facts_keep_native_continuation_separate_from_history() {
    let body = json!({"max_output_tokens":123,"input":[],"previous_response_id":"resp_1"});
    let facts = request_facts("openai", body.as_object().expect("请求正文"));
    assert_eq!(facts.max_output_tokens, Some(123));
    assert!(facts.native_continuation);
    assert!(!facts.tools && !facts.vision && !facts.reasoning && !facts.json_schema);
}
