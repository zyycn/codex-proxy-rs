//! Responses 生成参数不继承 Messages 的默认值、推理约束和采样裁剪

use gateway_protocol::genai::responses_to_genai_request;
use serde_json::{Value, json};

#[test]
fn unspecified_generation_settings_remain_unspecified_on_tool_continuations() {
    let function = json!({"type":"function","name":"lookup","parameters":{"type":"object"}});
    for choice in [json!("auto"), json!("required"), function.clone()] {
        let mut request = json!({"tools":[function],"tool_choice":choice,"input":[
            {"type":"function_call","call_id":"c","name":"lookup","arguments":"{}"},
            {"type":"function_call_output","call_id":"c","output":"value"}
        ]});
        for maximum in [None, Some(Value::Null)] {
            if let Some(maximum) = maximum {
                request["max_output_tokens"] = maximum;
            }
            let body = responses_to_genai_request(request.as_object().unwrap()).unwrap();
            assert_eq!(body["generationConfig"], json!({}));
            assert_eq!(
                body["contents"][1]["parts"][0]["functionResponse"]["name"],
                "lookup"
            );
        }
    }
}

#[test]
fn explicit_reasoning_and_sampling_survive_required_tools_and_small_output_limits() {
    let function = json!({"type":"function","name":"lookup","parameters":{"type":"object"}});
    for choice in [
        json!("required"),
        function.clone(),
        json!({"type":"allowed_tools","mode":"required","tools":[{"type":"function","name":"lookup"}]}),
    ] {
        let request = json!({"input":"hello","tools":[function],"tool_choice":choice,
            "max_output_tokens":128,"reasoning":{"effort":"high"},"temperature":0.3,"top_p":0.8});
        let body = responses_to_genai_request(request.as_object().unwrap()).unwrap();
        assert_eq!(
            body["generationConfig"],
            json!({"maxOutputTokens":128,"thinkingConfig":{"thinkingBudget":24576,"includeThoughts":true},"temperature":0.3,"topP":0.8})
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        if choice["type"] == "function" {
            assert_eq!(
                body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
                json!(["lookup"])
            );
        }
    }
}

#[test]
fn reasoning_budgets_do_not_inherit_messages_limits() {
    for (effort, budget) in [
        ("none", 0),
        ("minimal", 1024),
        ("low", 1024),
        ("medium", 8192),
        ("high", 24576),
        ("xhigh", 32768),
        ("max", 128000),
        ("auto", -1),
    ] {
        let request = json!({"input":"hello","reasoning":{"effort":effort}});
        let body = responses_to_genai_request(request.as_object().unwrap()).unwrap();
        let thinking = &body["generationConfig"]["thinkingConfig"];
        assert_eq!(thinking["thinkingBudget"], budget, "{effort}");
        assert_eq!(
            thinking.get("includeThoughts"),
            (budget != 0).then_some(&json!(true))
        );
        assert!(body["generationConfig"].get("maxOutputTokens").is_none());
    }
}

#[test]
fn explicit_output_limits_must_be_positive_integers() {
    for maximum in [json!(0), json!(-1), json!(1.5), json!("128"), json!(true)] {
        let request = json!({"input":"hello","max_output_tokens":maximum});
        assert!(
            responses_to_genai_request(request.as_object().unwrap()).is_err(),
            "{maximum}"
        );
    }
    for maximum in [1, 128, 100000] {
        let request = json!({"input":"hello","max_output_tokens":maximum});
        let body = responses_to_genai_request(request.as_object().unwrap()).unwrap();
        assert_eq!(body["generationConfig"]["maxOutputTokens"], maximum);
    }
}

#[test]
fn content_and_json_schema_preserve_their_target_representation() {
    let request = json!({"instructions":"system","input":[{"role":"user","content":[
        {"type":"input_text","text":"hello"},
        {"type":"input_image","image_url":"data:image/png;base64,AA=="}
    ]}],"text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object","properties":{"answer":{"type":"string"}}}}}});
    let body = responses_to_genai_request(request.as_object().unwrap()).unwrap();
    assert_eq!(
        body["systemInstruction"]["parts"],
        json!([{"text":"system"}])
    );
    assert_eq!(
        body["contents"][0]["parts"],
        json!([{"text":"hello"},{"inlineData":{"mimeType":"image/png","data":"AA=="}}])
    );
    assert_eq!(
        body["generationConfig"],
        json!({"responseMimeType":"application/json","responseSchema":request["text"]["format"]["schema"]})
    );
}

#[test]
fn parallel_tool_results_keep_call_order_and_native_media() {
    let request = json!({"input":[
        {"type":"custom_tool_call","call_id":"a","name":"display","namespace":"screen","input":"shot"},
        {"type":"function_call","call_id":"b","name":"lookup","arguments":"{}"},
        {"type":"function_call_output","call_id":"b","output":{"value":2}},
        {"type":"custom_tool_call_output","call_id":"a","output":"stale"},
        {"type":"custom_tool_call_output","call_id":"a","status":"incomplete","output":[
            {"type":"input_text","text":"captured"},
            {"type":"input_image","image_url":"data:image/png;base64,AA=="},
            {"type":"input_file","file_data":"data:application/pdf;base64,AQ=="}
        ]}
    ], "tools":[{"type":"namespace","name":"screen","tools":[{"type":"custom","name":"display"}]}],
        "tool_choice":{"type":"custom","name":"display","namespace":"screen"}});
    let body = responses_to_genai_request(request.as_object().unwrap()).unwrap();
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
        json!(["screen__display"])
    );
    assert_eq!(
        body["contents"][1]["parts"],
        json!([
            {"functionResponse":{"id":"a","name":"screen__display","response":{"error":"captured"},"parts":[
                {"inlineData":{"mimeType":"image/png","data":"AA=="}},{"inlineData":{"mimeType":"application/pdf","data":"AQ=="}}
            ]}},
            {"functionResponse":{"id":"b","name":"lookup","response":{"output":{"value":2}}}}
        ])
    );
}

#[test]
fn late_tool_results_remain_in_user_order_and_do_not_close_earlier_turns() {
    let request = json!({"input":[
        {"type":"function_call","name":"tool","call_id":"a","arguments":"{}"},
        {"role":"user","content":"continue"},
        {"role":"assistant","content":"ok"},
        {"role":"user","content":"before"},
        {"type":"reasoning","summary":[],"encrypted_content":"opaque-other-model"},
        {"type":"function_call_output","call_id":"a","output":[{"type":"input_text","text":"late"}]},
        {"role":"user","content":"after"}
    ]});
    let body = responses_to_genai_request(request.as_object().unwrap()).unwrap();
    assert!(body["contents"][1]["parts"][0]["functionResponse"]["response"]["error"].is_string());
    assert_eq!(
        body["contents"][3]["parts"],
        json!([{"text":"before"},{"text":"late"},{"text":"after"}])
    );
}

#[test]
fn named_tool_selection_requires_the_declared_type_and_namespace() {
    let function = json!({"type":"function","name":"read"});
    for choice in [
        json!({"type":"function","name":"read","namespace":"missing"}),
        json!({"type":"custom","name":"read"}),
    ] {
        let request = json!({"input":"read","tools":[function],"tool_choice":choice});
        assert!(responses_to_genai_request(request.as_object().unwrap()).is_err());
    }
}
