//! 验证不同协议的共享请求事实

use gateway_protocol::request::{request_facts, request_observation};
use serde_json::{Map, json};

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

#[test]
fn image_generation_detection_distinguishes_tools_from_image_inputs_and_passive_namespaces() {
    for body in [
        json!({"tools":[{"type":"image_generation"}]}),
        json!({"tool_choice":{"type":"image_generation"}}),
        json!({"tool_choice":{"type":"allowed_tools","tools":[{"type":"image_generation"}]}}),
        json!({"input":[{"type":"additional_tools","tools":[{"type":"image_generation"}]}]}),
    ] {
        assert!(
            request_facts("openai", body.as_object().expect("请求正文")).image_generation,
            "{body}"
        );
    }
    for body in [
        json!({"input":"Please generate an image"}),
        json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,AA=="}]}]}),
        json!({"tools":[{"type":"namespace","name":"image_gen","tools":[{"type":"function","name":"imagegen"}]}]}),
        json!({"tools":[{"type":"function","name":"image_generation"}]}),
    ] {
        assert!(
            !request_facts("openai", body.as_object().expect("请求正文")).image_generation,
            "{body}"
        );
    }
    let body = json!({"tools":[{"type":"image_generation"}]});
    for protocol in ["anthropic", "private"] {
        assert!(!request_facts(protocol, body.as_object().expect("请求正文")).image_generation);
    }
}

#[test]
fn messages_request_observation_preserves_explicit_effort_without_inventing_defaults() {
    for (options, expected) in [
        (json!({"output_config":{"effort":"xhigh"}}), Some("xhigh")),
        (
            json!({"thinking":{"type":"adaptive"},"output_config":{"effort":"low"}}),
            Some("low"),
        ),
        (
            json!({"thinking":{"type":"disabled"},"output_config":{"effort":"high"}}),
            Some("high"),
        ),
        (
            json!({"output_config":{"effort":" Future-Level "}}),
            Some("Future-Level"),
        ),
        (json!({"output_config":{"effort":" "}}), None),
        (json!({"output_config":{"effort":null}}), None),
        (json!({"output_config":{"effort":42}}), None),
        (json!({"thinking":{"type":"adaptive"}}), None),
        (
            json!({"thinking":{"type":"enabled","budget_tokens":32768}}),
            None,
        ),
        (json!({}), None),
    ] {
        let mut body = json!({"model":"gpt-5.4","max_tokens":128,"messages":[{"role":"user","content":"hello"}]});
        body.as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        let observation = request_observation(
            "anthropic",
            body.as_object().unwrap(),
            &Map::new(),
            Some("inapplicable-previous-effort"),
        );
        assert_eq!(
            observation.reasoning_effort.as_deref(),
            expected,
            "{options}"
        );
    }
}
