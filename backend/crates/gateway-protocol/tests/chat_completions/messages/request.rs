//! Chat 到 Messages 的工具、媒体、能力和生成设置合同

use gateway_protocol::{
    anthropic::MessagesModelCapabilities, chat_completions::messages::convert_request,
};
use serde_json::json;

#[test]
fn maps_multimodal_conversation_and_tool_results_directly() {
    let request = json!({"model":"claude","messages":[{"role":"system","content":"brief"},{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}},{"type":"file","file":{"filename":"a.pdf","file_data":"data:application/pdf;base64,QQ=="}}]},{"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"function","function":{"name":"lookup","arguments":"{\"q\":1}"}},{"id":"b","type":"custom","custom":{"name":"shell","input":"pwd"}}]},{"role":"tool","tool_call_id":"a","content":"result"},{"role":"tool","tool_call_id":"b","content":"directory"}],"tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}},{"type":"custom","custom":{"name":"shell"}}],"parallel_tool_calls":false});
    let result = convert_request(
        request.as_object().unwrap(),
        &MessagesModelCapabilities::default(),
    )
    .unwrap();
    assert_eq!(result["system"][0]["text"], "brief");
    assert_eq!(
        result["messages"][0]["content"][0]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(result["messages"][0]["content"][1]["type"], "document");
    assert_eq!(
        result["messages"][1]["content"][1]["input"],
        json!({"input":"pwd"})
    );
    assert_eq!(
        result["messages"][2]["content"].as_array().unwrap().len(),
        2
    );
    assert_eq!(result["messages"][2]["content"][0]["tool_use_id"], "a");
    assert_eq!(
        result["tool_choice"],
        json!({"type":"auto","disable_parallel_tool_use":true})
    );
    assert!(result.get("input").is_none());
}

#[test]
fn model_capabilities_drive_budget_and_adaptive_reasoning() {
    let mut request = super::super::request();
    request.insert("reasoning_effort".into(), json!("high"));
    request.insert("max_completion_tokens".into(), json!(5000));
    request.insert(
        "response_format".into(),
        json!({"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"}}}),
    );
    request.insert("temperature".into(), json!(0.8));
    let capabilities = MessagesModelCapabilities {
        adaptive_thinking: Some(true),
        max_tokens: Some(4000),
        efforts: Some(vec!["high".into()]),
        ..Default::default()
    };
    let result = convert_request(&request, &capabilities).unwrap();
    assert_eq!(result["max_tokens"], 4000);
    assert_eq!(result["thinking"], json!({"type":"adaptive"}));
    assert_eq!(
        result["output_config"],
        json!({"effort":"high","format":{"type":"json_schema","schema":{"type":"object"}}})
    );
    assert!(!result.contains_key("temperature"));
}

#[test]
fn forced_tool_and_reasoning_cannot_silently_change_user_policy() {
    let mut request = super::super::request();
    request.insert("reasoning_effort".into(), json!("high"));
    request.insert(
        "tools".into(),
        json!([{"type":"function","function":{"name":"f"}}]),
    );
    request.insert("tool_choice".into(), json!("required"));
    let error = convert_request(&request, &MessagesModelCapabilities::default()).unwrap_err();
    assert_eq!(error.parameter(), Some("reasoning_effort"));
}

#[test]
fn hosted_search_is_only_enabled_by_selected_model_capability() {
    let mut request = super::super::request();
    request.insert(
        "web_search_options".into(),
        json!({"user_location":{"type":"approximate","approximate":{"city":"Paris"}}}),
    );
    assert!(convert_request(&request, &MessagesModelCapabilities::default()).is_err());
    let capabilities = MessagesModelCapabilities {
        hosted_web_search: Some(true),
        ..Default::default()
    };
    let result = convert_request(&request, &capabilities).unwrap();
    assert_eq!(
        result["tools"][0]["user_location"],
        json!({"type":"approximate","city":"Paris"})
    );
    assert_eq!(
        result["tool_choice"],
        json!({"type":"tool","name":"web_search"})
    );
}

#[test]
fn orphan_results_and_unrepresentable_audio_are_rejected() {
    for messages in [
        json!([{"role":"tool","tool_call_id":"orphan","content":"result"}]),
        json!([{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"AA==","format":"wav"}}]}]),
    ] {
        let mut request = super::super::request();
        request.insert("messages".into(), messages);
        assert!(convert_request(&request, &MessagesModelCapabilities::default()).is_err());
    }
}
