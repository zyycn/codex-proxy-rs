//! Chat 到 GenAI 直接请求支持多模态和工具轮次

use gateway_protocol::chat_completions::genai::convert_request;
use serde_json::json;

#[test]
fn maps_system_media_and_tools_without_an_intermediate_platform() {
    let chat = json!({"model":"gemini","messages":[{"role":"developer","content":"brief"},{"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}},{"type":"input_audio","input_audio":{"data":"QQ==","format":"mp3"}},{"type":"file","file":{"filename":"a.pdf","file_data":"data:application/pdf;base64,QQ=="}}]},{"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"custom","custom":{"name":"shell","input":"pwd"}}]},{"role":"tool","tool_call_id":"a","content":"directory"}],"tools":[{"type":"custom","custom":{"name":"shell"}}],"tool_choice":"required","max_completion_tokens":256,"reasoning_effort":"low"});
    let result = convert_request(chat.as_object().unwrap()).unwrap();
    assert_eq!(result["systemInstruction"]["parts"][0]["text"], "brief");
    assert_eq!(
        result["contents"][0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(
        result["contents"][0]["parts"][2]["inlineData"]["mimeType"],
        "audio/mpeg"
    );
    assert_eq!(
        result["contents"][0]["parts"][3]["inlineData"]["mimeType"],
        "application/pdf"
    );
    assert_eq!(
        result["contents"][1]["parts"][0]["functionCall"]["args"],
        json!({"input":"pwd"})
    );
    assert_eq!(
        result["contents"][1]["parts"][0]["thoughtSignature"],
        "skip_thought_signature_validator"
    );
    assert_eq!(
        result["contents"][2]["parts"][0]["functionResponse"]["name"],
        "shell"
    );
    assert_eq!(result["generationConfig"]["maxOutputTokens"], 256);
    assert_eq!(
        result["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        1024
    );
    assert_eq!(
        result["toolConfig"]["functionCallingConfig"],
        json!({"mode":"ANY","allowedFunctionNames":["shell"]})
    );
    for field in ["messages", "input", "max_tokens"] {
        assert!(!result.contains_key(field));
    }
}

#[test]
fn structured_output_preserves_json_schema_and_explicit_logprobs() {
    let mut request = super::super::request();
    request.insert("response_format".into(),json!({"type":"json_schema","json_schema":{"name":"result","schema":{"type":"object","additionalProperties":false,"properties":{"kind":{"const":"x"}}}}}));
    request.insert("logprobs".into(), json!(true));
    request.insert("top_logprobs".into(), json!(2));
    let result = convert_request(&request).unwrap();
    assert_eq!(
        result["generationConfig"]["responseJsonSchema"],
        request["response_format"]["json_schema"]["schema"]
    );
    assert_eq!(result["generationConfig"]["responseLogprobs"], true);
    assert_eq!(result["generationConfig"]["logprobs"], 2);
}

#[test]
fn hosted_search_is_generated_directly_and_constraints_are_not_silently_dropped() {
    let mut request = super::super::request();
    request.insert("web_search_options".into(), json!({}));
    assert_eq!(
        convert_request(&request).unwrap()["tools"],
        json!([{"googleSearch":{}}])
    );
    request["web_search_options"]["user_location"] =
        json!({"type":"approximate","approximate":{"city":"Paris"}});
    assert!(convert_request(&request).is_err());
}

#[test]
fn orphan_tool_results_and_untyped_remote_media_are_rejected() {
    for messages in [
        json!([{"role":"tool","tool_call_id":"orphan","content":"result"}]),
        json!([{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.com/image"}}]}]),
    ] {
        let mut request = super::super::request();
        request.insert("messages".into(), messages);
        assert!(convert_request(&request).is_err());
    }
}

#[test]
fn search_context_without_a_genai_equivalent_is_not_silently_ignored() {
    for size in ["low", "medium", "high"] {
        let mut request = super::super::request();
        request.insert(
            "web_search_options".into(),
            json!({"search_context_size":size}),
        );
        assert_eq!(
            convert_request(&request).unwrap_err().parameter(),
            Some("web_search_options.search_context_size")
        );
    }
}
