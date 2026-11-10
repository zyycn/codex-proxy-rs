//! 验证请求内容、工具配对和签名来源

use gateway_protocol::anthropic::{
    messages_to_responses_count_request, messages_to_responses_request,
    messages_to_responses_response, responses_to_messages_request, responses_to_messages_response,
    validate_count_request, validate_request,
};
use serde_json::{Map, Value, json};

use gateway_protocol::anthropic::{
    strip_foreign_reasoning_for_responses, strip_foreign_thinking_for_messages,
};

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("测试正文必须是对象").clone()
}

#[test]
fn mid_conversation_system_messages_keep_their_role_and_position() {
    let request = object(json!({
        "model":"claude", "max_tokens":128,
        "messages":[
            {"role":"user", "content":"hello"},
            {"role":"system", "content":"Be brief."},
            {"role":"system", "content":[], "output_config":{"effort":"medium"}}
        ]
    }));
    assert!(validate_request(&request).is_ok());
    let converted = messages_to_responses_request(&request).expect("转换会话中的 system 消息");
    assert_eq!(converted["input"].as_array().unwrap().len(), 2);
    assert_eq!(converted["input"][0]["role"], "user");
    assert_eq!(converted["input"][1]["role"], "system");
    assert_eq!(converted["input"][1]["content"][0]["text"], "Be brief.");
}

#[test]
fn counting_does_not_require_or_invent_generation_budget() {
    let request = object(json!({"model":"model","messages":[{"role":"user","content":"hello"}]}));
    assert!(validate_count_request(&request).is_ok());
    assert!(validate_request(&request).is_err());
    let converted = messages_to_responses_count_request(&request).expect("计数转换");
    assert!(!converted.contains_key("max_output_tokens"));
    assert_eq!(converted["input"][0]["content"][0]["text"], "hello");
}

#[test]
fn system_images_tools_and_structured_output_map_without_losing_order() {
    let request = object(
        json!({"model":"claude","max_tokens":4096,"system":"brief","messages":[{"role":"user","content":[{"type":"text","text":"look"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}]},{"role":"assistant","content":[{"type":"text","text":"calling"},{"type":"tool_use","id":"call_1","name":"lookup","input":{"q":"x"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"found"}]}],"tools":[{"name":"lookup","input_schema":{"type":"object"}}],"tool_choice":{"type":"tool","name":"lookup","disable_parallel_tool_use":true},"output_config":{"format":{"type":"json_schema","schema":{"type":"object"}}}}),
    );
    let converted = messages_to_responses_request(&request).expect("转换");
    assert_eq!(converted["input"][0]["role"], "developer");
    assert_eq!(
        converted["input"][1]["content"][1]["image_url"],
        "data:image/png;base64,AA=="
    );
    assert_eq!(converted["input"][3]["type"], "function_call");
    assert_eq!(converted["input"][4]["output"], "found");
    assert_eq!(converted["parallel_tool_calls"], false);
    assert_eq!(converted["text"]["format"]["name"], "response");
}

#[test]
fn custom_tools_round_trip_and_orphan_outputs_follow_cpa_fallback() {
    let request = object(
        json!({"model":"model","tools":[{"type":"custom","name":"shell","description":"run a command"}],"input":[{"type":"custom_tool_call","call_id":"call_1","name":"shell","input":"echo ok"},{"type":"custom_tool_call_output","call_id":"call_1","output":"old"},{"type":"custom_tool_call_output","call_id":"call_1","output":"new"},{"type":"function_call_output","call_id":"missing","output":"orphan"}]}),
    );
    let converted = responses_to_messages_request(&request, &Default::default()).expect("转换");
    assert_eq!(
        converted["messages"][0]["content"][0]["input"]["input"],
        "echo ok"
    );
    assert_eq!(converted["messages"][1]["content"][0]["content"], "new");
    assert_eq!(converted["messages"][1]["content"][1]["text"], "orphan");
    let native = json!({"id":"m1","content":[{"type":"tool_use","id":"call_2","name":"shell","input":{"input":"pwd"}}],"stop_reason":"tool_use","usage":{}});
    let response = messages_to_responses_response(&native, &request).expect("返回转换");
    assert_eq!(response["output"][0]["type"], "custom_tool_call");
    assert_eq!(response["output"][0]["input"], "pwd");
}

#[test]
fn parallel_tool_results_precede_text_and_keep_custom_input_and_errors() {
    let request = object(json!({
        "model":"model",
        "tools":[
            {"type":"function","name":"read","parameters":{"type":"object"}},
            {"type":"custom","name":"patch"}
        ],
        "input":[
            {"type":"function_call","call_id":"read_1","name":"read","arguments":"{}"},
            {"type":"custom_tool_call","call_id":"patch_1","name":"patch","input":"patch text"},
            {"role":"user","content":"before"},
            {"type":"custom_tool_call_output","call_id":"patch_1","output":"patch failed","status":"incomplete"},
            {"role":"user","content":"between"},
            {"type":"function_call_output","call_id":"read_1","output":"read result"}
        ]
    }));
    let converted = responses_to_messages_request(&request, &Default::default()).unwrap();
    assert_eq!(
        converted["messages"][0]["content"][1]["input"],
        json!({"input":"patch text"})
    );
    assert_eq!(
        converted["messages"][1]["content"],
        json!([
            {"type":"tool_result","tool_use_id":"patch_1","content":"patch failed","is_error":true},
            {"type":"tool_result","tool_use_id":"read_1","content":"read result"},
            {"type":"text","text":"before"},
            {"type":"text","text":"between"}
        ])
    );
}

#[test]
fn interrupted_parallel_calls_close_before_the_conversation_continues() {
    let request = object(json!({"model":"model","input":[
        {"type":"function_call","call_id":"first","name":"read","arguments":"{}"},
        {"type":"function_call","call_id":"second","name":"read","arguments":"{}"},
        {"type":"function_call_output","call_id":"first","output":"available"},
        {"role":"user","content":"continue without waiting"},
        {"role":"assistant","content":"continuing"},
        {"type":"function_call_output","call_id":"second","output":"late result"}
    ]}));
    let converted = responses_to_messages_request(&request, &Default::default()).unwrap();
    assert_eq!(
        converted["messages"][1]["content"],
        json!([
            {"type":"tool_result","tool_use_id":"second","is_error":true,"content":"Tool call was interrupted before any output was recorded."},
            {"type":"tool_result","tool_use_id":"first","content":"available"},
            {"type":"text","text":"continue without waiting"}
        ])
    );
    assert_eq!(converted["messages"][2]["content"][0]["text"], "continuing");
    assert_eq!(
        converted["messages"][3]["content"],
        json!([
            {"type":"text","text":"late result"}
        ])
    );
}

#[test]
fn trailing_tool_calls_get_explicit_interrupted_results() {
    let request = object(json!({"model":"model","input":[
        {"role":"user","content":"read files"},
        {"type":"function_call","call_id":"second","name":"read","arguments":"{}"},
        {"type":"function_call","call_id":"first","name":"read","arguments":"{}"}
    ]}));
    let converted = responses_to_messages_request(&request, &Default::default()).unwrap();
    assert_eq!(converted["messages"][2]["role"], "user");
    let results = converted["messages"][2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "second");
    assert_eq!(results[1]["tool_use_id"], "first");
    assert!(results.iter().all(|result| result["is_error"] == true));
}

#[test]
fn duplicate_results_do_not_break_an_earlier_completed_round() {
    let request = object(json!({"model":"model","input":[
        {"type":"function_call","call_id":"call","name":"read","arguments":"{}"},
        {"type":"function_call_output","call_id":"call","output":"old"},
        {"type":"function_call_output","call_id":"call","output":"new"},
        {"role":"assistant","content":"completed"},
        {"type":"function_call_output","call_id":"call","output":"late duplicate"}
    ]}));
    let converted = responses_to_messages_request(&request, &Default::default()).unwrap();
    assert_eq!(
        converted["messages"][1]["content"],
        json!([
            {"type":"tool_result","tool_use_id":"call","content":"new"}
        ])
    );
    assert_eq!(
        converted["messages"][3]["content"],
        json!([
            {"type":"text","text":"late duplicate"}
        ])
    );
}

#[test]
fn orphan_results_reuse_multimodal_content_and_empty_results_keep_a_marker() {
    let request = object(json!({"model":"model","input":[
        {"type":"function_call_output","call_id":"missing","output":[
            {"type":"input_text","text":" "},
            {"type":"input_image","image_url":"data:image/png;base64,AA=="},
            {"type":"input_file","file_url":"https://example.com/file.pdf"},
            {"type":"input_text","text":"caption"}
        ]},
        {"type":"function_call_output","call_id":"empty","output":""}
    ]}));
    let converted = responses_to_messages_request(&request, &Default::default()).unwrap();
    assert_eq!(
        converted["messages"][0]["content"],
        json!([
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}},
            {"type":"document","source":{"type":"url","url":"https://example.com/file.pdf"}},
            {"type":"text","text":"caption"},
            {"type":"text","text":"Tool result was empty."}
        ])
    );
}

#[test]
fn closed_tool_history_is_unchanged_after_another_round_trip() {
    let request = object(json!({"model":"model","input":[
        {"role":"user","content":"read"},
        {"type":"function_call","call_id":"call","name":"read","arguments":"{\"path\":\"file\"}"},
        {"type":"function_call_output","call_id":"call","output":"result"},
        {"role":"user","content":"summarize"},
        {"role":"assistant","content":"summary"}
    ]}));
    let converted = responses_to_messages_request(&request, &Default::default()).unwrap();
    let replay = messages_to_responses_request(&converted).unwrap();
    let restored = responses_to_messages_request(&replay, &Default::default()).unwrap();
    assert_eq!(restored["messages"], converted["messages"]);
    assert_eq!(
        converted["messages"][2]["content"],
        json!([
            {"type":"tool_result","tool_use_id":"call","content":"result"},
            {"type":"text","text":"summarize"}
        ])
    );
}

#[test]
fn anthropic_thinking_and_redacted_blocks_return_to_their_origin() {
    let request = object(json!({"model":"model","input":"hi"}));
    let blocks = json!([{"type":"thinking","thinking":"private","signature":"signed-native"},{"type":"redacted_thinking","data":"opaque"}]);
    let response = messages_to_responses_response(
        &json!({"id":"m1","content":blocks,"stop_reason":"end_turn","usage":{}}),
        &request,
    )
    .expect("响应转换");
    let mut followup = request;
    followup.insert("input".into(), response["output"].clone());
    let restored = responses_to_messages_request(&followup, &Default::default()).expect("回传转换");
    assert_eq!(restored["messages"][0]["content"], blocks);
}

#[test]
fn openai_reasoning_returns_to_its_origin_and_foreign_signatures_are_dropped() {
    let request = object(
        json!({"model":"model","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}),
    );
    let item = json!({"type":"reasoning","id":"r1","summary":[{"type":"summary_text","text":"reason"}],"encrypted_content":"gAAAAopaque"});
    let response = responses_to_messages_response(
        &json!({"id":"r0","status":"completed","output":[item],"usage":{}}),
        &request,
    )
    .expect("响应转换");
    let mut followup = request;
    followup.insert(
        "messages".into(),
        json!([{"role":"assistant","content":response["content"]}]),
    );
    let restored = messages_to_responses_request(&followup).expect("回传转换");
    assert_eq!(restored["input"][0], item);
    followup.insert("messages".into(),json!([{"role":"assistant","content":[{"type":"thinking","thinking":"thought","signature":"foreign-anthropic"},{"type":"text","text":"answer"}]}]));
    let foreign = messages_to_responses_request(&followup).expect("外来源降级");
    assert_eq!(foreign["input"].as_array().expect("数组").len(), 1);
    assert_eq!(foreign["input"][0]["type"], "message");
}

#[test]
fn reasoning_budget_stays_inside_explicit_output_limit() {
    let request = object(
        json!({"model":"model","input":"hi","max_output_tokens":2048,"reasoning":{"effort":"high"}}),
    );
    let converted = responses_to_messages_request(&request, &Default::default()).expect("预算转换");
    assert_eq!(
        converted["thinking"],
        json!({"type":"enabled","budget_tokens":2047})
    );
    assert_eq!(converted["max_tokens"], 2048);
}

#[test]
fn switching_to_native_messages_only_removes_our_openai_carrier() {
    let source = object(
        json!({"model":"model","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}),
    );
    let response=responses_to_messages_response(&json!({"id":"r1","status":"completed","output":[{"type":"reasoning","id":"reason1","summary":[{"type":"summary_text","text":"thinking"}],"encrypted_content":"gAAAAopaque"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}],"usage":{}}),&source).expect("第一次走Responses上游");
    let mut next = source;
    let native = json!({"type":"thinking","thinking":"native","signature":"unknown-native-signature","future_field":9});
    next.insert("messages".into(),json!([{"role":"assistant","content":response["content"]},{"role":"assistant","content":[native]},{"role":"user","content":"continue"}]));
    strip_foreign_thinking_for_messages(&mut next);
    assert_eq!(
        next["messages"][0]["content"],
        json!([{"type":"text","text":"answer"}])
    );
    assert_eq!(next["messages"][1]["content"][0], native);
    assert_eq!(next["messages"][2]["content"], "continue");
}

#[test]
fn switching_to_native_responses_only_removes_our_anthropic_carrier() {
    let source = object(json!({"model":"model","input":"hi"}));
    let response=messages_to_responses_response(&json!({"id":"m1","content":[{"type":"thinking","thinking":"thought","signature":"native"},{"type":"text","text":"answer"}],"stop_reason":"end_turn","usage":{}}),&source).expect("第一次走Messages上游");
    let mut next = source;
    let native = json!({"type":"reasoning","encrypted_content":"unknown-native","future_field":7});
    let mut input = response["output"].as_array().expect("输出数组").clone();
    input.push(native.clone());
    next.insert("input".into(), json!(input));
    strip_foreign_reasoning_for_responses(&mut next);
    assert_eq!(next["input"].as_array().expect("输入数组").len(), 2);
    assert_eq!(next["input"][0]["type"], "message");
    assert_eq!(next["input"][1], native);
}

#[test]
fn aliased_tool_names_never_overwrite_an_existing_declaration() {
    let source = object(
        json!({"model":"model","input":"hi","tools":[{"type":"function","name":"invalid.name","parameters":{}},{"type":"function","name":"tool_0","parameters":{}}]}),
    );
    let converted = responses_to_messages_request(&source, &Default::default()).expect("工具别名");
    assert_eq!(converted["tools"].as_array().expect("工具数组").len(), 2);
    let wire = converted["tools"][1]["name"].as_str().expect("工具名称");
    let response = messages_to_responses_response(
        &json!({"content":[{"type":"tool_use","id":"c1","name":wire,"input":{}}],"usage":{}}),
        &source,
    )
    .expect("反向工具别名");
    assert_eq!(response["output"][0]["name"], "tool_0");
}

#[test]
fn explicit_thinking_switches_and_cpa_budget_levels_survive_conversion() {
    for (effort, budget) in [
        ("minimal", 1024),
        ("low", 1024),
        ("medium", 8192),
        ("high", 24576),
        ("xhigh", 32768),
        ("max", 128000),
    ] {
        let request = object(
            json!({"model":"model","input":"hi","max_output_tokens":200000,"reasoning":{"effort":effort}}),
        );
        let converted = responses_to_messages_request(&request, &Default::default()).unwrap();
        assert_eq!(
            converted["thinking"],
            json!({"type":"enabled","budget_tokens":budget})
        );
    }
    let disabled = object(json!({"model":"model","input":"hi","reasoning":{"effort":"none"}}));
    assert_eq!(
        responses_to_messages_request(&disabled, &Default::default()).unwrap()["thinking"],
        json!({"type":"disabled"})
    );
    let no_budget = object(
        json!({"model":"model","input":"hi","max_output_tokens":1024,"reasoning":{"effort":"high"}}),
    );
    assert_eq!(
        responses_to_messages_request(&no_budget, &Default::default()).unwrap()["thinking"],
        json!({"type":"disabled"})
    );
    for (thinking, effort) in [
        (json!({"type":"disabled"}), "none"),
        (json!({"type":"enabled","budget_tokens":1024}), "low"),
        (json!({"type":"enabled","budget_tokens":8192}), "medium"),
        (json!({"type":"enabled","budget_tokens":24576}), "high"),
        (json!({"type":"enabled","budget_tokens":32768}), "xhigh"),
    ] {
        let request = object(
            json!({"model":"model","messages":[{"role":"user","content":"hi"}],"max_tokens":200000,"thinking":thinking}),
        );
        assert_eq!(
            messages_to_responses_request(&request).unwrap()["reasoning"]["effort"],
            effort
        );
    }
    for thinking in [None, Some(json!({"type":"adaptive"}))] {
        let mut request = object(
            json!({"model":"model","messages":[{"role":"user","content":"hi"}],"max_tokens":1024,"output_config":{"effort":"low"}}),
        );
        if let Some(thinking) = thinking {
            request.insert("thinking".into(), thinking);
        }
        assert_eq!(
            messages_to_responses_request(&request).unwrap()["reasoning"]["effort"],
            "low"
        );
    }
}

#[test]
fn adaptive_capabilities_drive_thinking_effort_and_output_limit_without_model_guessing() {
    let capabilities = gateway_protocol::anthropic::MessagesModelCapabilities::from_model(&json!({
        "max_tokens":512,
        "capabilities":{"thinking":{"supported":true,"types":{"adaptive":{"supported":true},"enabled":{"supported":false},"disabled":{"supported":false}}},"effort":{"low":{"supported":true},"high":{"supported":true}}}
    }));
    for (effort, expected) in [
        ("none", Some("low")),
        ("minimal", Some("low")),
        ("medium", Some("low")),
        ("high", Some("high")),
        ("max", Some("high")),
        ("auto", None),
    ] {
        let source = object(
            json!({"model":"arbitrary-alias","input":"hello","max_output_tokens":1000,"temperature":0.3,"top_p":0.9,"reasoning":{"effort":effort},"text":{"format":{"type":"json_schema","schema":{"type":"object"}}}}),
        );
        let converted = responses_to_messages_request(&source, &capabilities).unwrap();
        assert_eq!(converted["thinking"], json!({"type":"adaptive"}));
        assert_eq!(converted["max_tokens"], 512);
        assert!(!converted.contains_key("temperature"));
        assert!(!converted.contains_key("top_p"));
        assert_eq!(
            converted["output_config"]
                .get("effort")
                .and_then(Value::as_str),
            expected
        );
        assert_eq!(
            converted["output_config"]["format"]["schema"]["type"],
            "object"
        );
    }
}

#[test]
fn forced_tools_respect_manual_and_adaptive_thinking_modes() {
    let source = object(
        json!({"model":"alias","input":"hello","tools":[{"type":"function","name":"echo","parameters":{"type":"object"}}],"reasoning":{"effort":"high"},"tool_choice":{"type":"function","name":"echo"},"parallel_tool_calls":false}),
    );
    let converted = responses_to_messages_request(&source, &Default::default()).unwrap();
    assert_eq!(
        converted["tool_choice"],
        json!({"type":"tool","name":"echo","disable_parallel_tool_use":true})
    );
    assert_eq!(converted["thinking"], json!({"type":"disabled"}));
    let adaptive = gateway_protocol::anthropic::MessagesModelCapabilities {
        adaptive_thinking: Some(true),
        disabled_thinking: Some(false),
        ..Default::default()
    };
    let converted = responses_to_messages_request(&source, &adaptive).unwrap();
    assert_eq!(
        converted["tool_choice"],
        json!({"type":"auto","disable_parallel_tool_use":true})
    );
    assert_eq!(converted["thinking"], json!({"type":"adaptive"}));
    assert_eq!(converted["output_config"]["effort"], "high");
}

#[test]
fn serial_tools_get_default_choice_but_none_has_no_parallel_option() {
    for (choice, expected) in [
        (
            None,
            json!({"type":"auto","disable_parallel_tool_use":true}),
        ),
        (Some(json!("none")), json!({"type":"none"})),
        (
            Some(json!("required")),
            json!({"type":"any","disable_parallel_tool_use":true}),
        ),
    ] {
        let mut source = object(
            json!({"input":"hello","tools":[{"type":"function","name":"echo"}],"parallel_tool_calls":false}),
        );
        if let Some(choice) = choice {
            source.insert("tool_choice".into(), choice);
        }
        assert_eq!(
            responses_to_messages_request(&source, &Default::default()).unwrap()["tool_choice"],
            expected
        );
        source.remove("tools");
        assert!(
            !responses_to_messages_request(&source, &Default::default())
                .unwrap()
                .contains_key("tool_choice")
        );
    }
}

#[test]
fn service_tier_and_speed_map_by_protocol_meaning() {
    for (tier, speed, target_tier) in [
        ("priority", Some("fast"), None),
        ("default", None, Some("standard_only")),
        ("auto", None, Some("auto")),
        ("flex", None, None),
    ] {
        let converted = responses_to_messages_request(
            &object(json!({"input":"hello","service_tier":tier})),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(converted.get("speed").and_then(Value::as_str), speed);
        assert_eq!(
            converted.get("service_tier").and_then(Value::as_str),
            target_tier
        );
    }
    for (tier, speed, expected) in [
        ("auto", None, "auto"),
        ("standard_only", None, "default"),
        ("standard_only", Some("fast"), "priority"),
    ] {
        let mut source = object(
            json!({"model":"alias","max_tokens":1000,"messages":[{"role":"user","content":"hello"}],"service_tier":tier}),
        );
        if let Some(speed) = speed {
            source.insert("speed".into(), json!(speed));
        }
        assert_eq!(
            messages_to_responses_request(&source).unwrap()["service_tier"],
            expected
        );
    }
}

#[test]
fn hosted_search_never_widens_network_access() {
    for access in [json!(false), json!("cached"), json!("indexed")] {
        let req = object(
            json!({"tools":[{"type":"web_search","name":"web_search","external_web_access":access}]}),
        );
        assert!(
            !responses_to_messages_request(&req, &Default::default())
                .unwrap()
                .contains_key("tools")
        );
    }
    let limited = object(
        json!({"model":"m","max_tokens":12,"messages":[{"role":"user","content":"hi"}],"tools":[{"type":"web_search_20250305","name":"web_search","max_uses":1}]}),
    );
    assert!(messages_to_responses_request(&limited).is_err());
    assert!(messages_to_responses_count_request(&limited).is_ok());
    for field in ["allowed_domains", "blocked_domains"] {
        let req = object(
            json!({"model":"m","max_tokens":12,"messages":[{"role":"user","content":"hi"}],"tools":[{"type":"web_search_20250305","name":"web_search",field:["docs.example"]}]}),
        );
        let converted = messages_to_responses_request(&req).unwrap();
        assert_eq!(
            converted["tools"][0]["filters"][field],
            json!(["docs.example"])
        );
        let back = responses_to_messages_request(&converted, &Default::default()).unwrap();
        assert_eq!(back["tools"], req["tools"]);
    }
}

#[test]
fn allowed_tools_limit_current_turn_without_renaming_history() {
    let mut req = object(
        json!({"tools":[{"type":"function","name":"delete"},{"type":"namespace","name":"fs","tools":[{"type":"function","name":"read"}]},{"type":"custom","name":"read"},{"type":"web_search"}],"tool_choice":{"type":"allowed_tools","mode":"required","tools":[{"type":"function","name":"read","namespace":"fs"}]},"parallel_tool_calls":false,"input":[{"type":"function_call","call_id":"c","name":"delete","arguments":"{}"},{"type":"function_call_output","call_id":"c","output":"done"}]}),
    );
    let converted = responses_to_messages_request(&req, &Default::default()).unwrap();
    assert_eq!(converted["tools"].as_array().unwrap().len(), 1);
    assert_eq!(converted["tools"][0]["name"], "fs__read");
    assert_eq!(
        converted["tool_choice"],
        json!({"type":"any","disable_parallel_tool_use":true})
    );
    assert_eq!(converted["messages"][0]["content"][0]["name"], "delete");
    req["tool_choice"]["tools"] = json!([{"type":"web_search"}]);
    req["tool_choice"]["mode"] = json!("auto");
    let converted = responses_to_messages_request(&req, &Default::default()).unwrap();
    assert_eq!(
        converted["tools"],
        json!([{"type":"web_search_20250305","name":"web_search"}])
    );
    req["tool_choice"]["tools"] = json!([{"type":"function","name":"missing"}]);
    assert!(responses_to_messages_request(&req, &Default::default()).is_err());
}

#[test]
fn tool_result_continuation_without_thinking_keeps_the_current_mode() {
    let mut req = object(json!({
        "tools":[{"type":"function","name":"read"}],
        "input":[{"type":"function_call","call_id":"c","name":"read","arguments":"{}"},
                 {"type":"function_call_output","call_id":"c","output":"ok"}]
    }));
    let convert =
        |req: &Map<String, Value>| responses_to_messages_request(req, &Default::default()).unwrap();
    assert_eq!(convert(&req)["thinking"], json!({"type":"disabled"}));
    req.insert("reasoning".into(), json!({"effort":"high"}));
    assert_eq!(convert(&req)["thinking"]["type"], "enabled");
    req.remove("reasoning");
    req["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"new question"}));
    assert!(!convert(&req).contains_key("thinking"));
}
