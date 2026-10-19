//! 验证 OAuth 搜索历史补偿的实际 HTTP/WS 出站、Lite 压缩及透传边界

use super::*;

fn search_request() -> Value {
    json!({
        "model":"gpt-5.4", "store":false, "stream":true,
        "input":[
            {"type":"message","role":"user","content":"Find the Rust website"},
            {"type":"web_search_call","id":"ws_history","status":"completed",
             "action":{"type":"search","query":"Rust official website"}},
            {"type":"message","role":"assistant","content":[
                {"type":"output_text","text":"https://www.rust-lang.org/",
                 "annotations":[{"type":"url_citation","url":"https://www.rust-lang.org/","title":"Rust","start_index":0,"end_index":26}]}]},
            {"type":"message","role":"user","content":"Repeat the domain"}
        ],
        "instructions":"Synthetic search history", "future_field":{"keep":true}
    })
}

fn cached_search() -> Value {
    json!({"type":"web_search","external_web_access":false})
}

fn echo_tool() -> Value {
    json!({"type":"function","name":"echo","parameters":{"type":"object"}})
}

fn assert_request_fields(actual: &Value, expected: &Value) {
    for field in [
        "input",
        "tools",
        "tool_choice",
        "instructions",
        "future_field",
    ] {
        assert_eq!(actual.get(field), expected.get(field), "{field}");
    }
}

#[tokio::test]
async fn search_history_without_tools_completes_with_cached_declaration_and_no_execution() {
    for websocket in [false, true] {
        for lite in [false, true] {
            for compact in [false, true] {
                let mut original = search_request();
                if compact {
                    original["input"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"type":"compaction_trigger"}));
                    original["tool_choice"] = json!("auto");
                    original["tools"] = json!([]);
                }
                let mut expected = original.clone();
                expected["tool_choice"] = json!("none");
                if lite {
                    let input = expected["input"].as_array_mut().unwrap();
                    input.insert(
                        input.len() - usize::from(compact),
                        json!({
                            "type":"additional_tools","role":"developer","tools":[cached_search()]
                        }),
                    );
                } else {
                    expected["tools"] = json!([cached_search()]);
                }
                let actual = capture_downstream_request(original, false, websocket, lite).await;
                assert_request_fields(&actual, &expected);
                // 将适配后的正文再次回放，不重复补入声明或改变引用
                let repeated =
                    capture_downstream_request(expected.clone(), false, websocket, lite).await;
                assert_request_fields(&repeated, &expected);
            }
        }
    }
}

#[tokio::test]
async fn search_history_preserves_existing_tools_and_explicit_choices() {
    for lite in [false, true] {
        for choice in [
            json!("auto"),
            json!("none"),
            json!("required"),
            json!({"type":"function","name":"echo"}),
        ] {
            let mut original = search_request();
            original["tool_choice"] = choice;
            if lite {
                original["input"].as_array_mut().unwrap().insert(0, json!({
                    "type":"additional_tools", "id":"at_original", "role":"developer", "tools":[echo_tool()]
                }));
            } else {
                original["tools"] = json!([echo_tool()]);
            }
            let mut expected = original.clone();
            if lite {
                expected["input"].as_array_mut().unwrap().push(json!({
                    "type":"additional_tools","role":"developer","tools":[cached_search()]
                }));
            } else {
                expected["tools"]
                    .as_array_mut()
                    .unwrap()
                    .push(cached_search());
            }
            let actual = capture_downstream_request(original, false, true, lite).await;
            assert_request_fields(&actual, &expected);
        }
    }
}

#[tokio::test]
async fn search_history_keeps_existing_search_declarations_in_all_supported_locations() {
    for kind in ["web_search", "web_search_preview"] {
        for location in ["top", "additional_tools", "tool_search_output"] {
            let mut original = search_request();
            let tool = json!({"type":kind,"external_web_access":true,"search_context_size":"high"});
            if location == "top" {
                original["tools"] = json!([tool]);
            } else {
                original["input"].as_array_mut().unwrap().insert(
                    0,
                    json!({
                        "type":location, "tools":[tool], "id":"existing_declaration"
                    }),
                );
            }
            let actual = capture_downstream_request(original.clone(), false, false, true).await;
            assert_request_fields(&actual, &original);
        }
    }
}

#[tokio::test]
async fn search_history_compensation_does_not_apply_to_api_key_accounts() {
    for websocket in [false, true] {
        for lite in [false, true] {
            let original = search_request();
            let actual = capture_downstream_request(original.clone(), true, websocket, lite).await;
            assert_request_fields(&actual, &original);
        }
    }
}

#[tokio::test]
async fn search_history_keeps_unrecognized_tools_and_unfulfillable_choices_unchanged() {
    let mut cases = Vec::new();
    for tools in [json!({"future":true}), json!("invalid")] {
        let mut body = search_request();
        body["tools"] = tools;
        cases.push(body);
    }
    for choice in [
        json!(null),
        json!("required"),
        json!("future-choice"),
        json!({"type":"function","name":"absent"}),
    ] {
        let mut body = search_request();
        body["tool_choice"] = choice;
        cases.push(body);
    }
    for kind in ["additional_tools", "tool_search_output"] {
        let mut body = search_request();
        body["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":kind,"tools":{"future":true}}));
        cases.push(body);
    }
    for body in cases {
        let actual = capture_downstream_request(body.clone(), false, false, true).await;
        assert_request_fields(&actual, &body);
    }
}

#[tokio::test]
async fn requests_without_hosted_search_history_do_not_gain_tools() {
    for input in [
        json!([{ "type":"message", "role":"user", "content":"web_search_call" }]),
        json!([{ "type":"custom_tool_call", "name":"web.run", "call_id":"call_search", "input":"{}" }]),
        json!([{ "type":"function_call_output", "call_id":"call_search", "output":{"type":"web_search_call"} }]),
        json!([]),
        json!(null),
        json!({"type":"web_search_call"}),
    ] {
        let mut original = search_request();
        original["input"] = input;
        let actual = capture_downstream_request(original.clone(), false, false, true).await;
        assert_request_fields(&actual, &original);
    }
}

#[tokio::test]
async fn search_history_does_not_infer_missing_declarations_from_native_continuation() {
    let mut original = search_request();
    original["previous_response_id"] = json!("resp_existing");
    original["store"] = json!(true);
    let actual = capture_downstream_request(original.clone(), false, true, true).await;
    assert_request_fields(&actual, &original);
    assert_eq!(
        actual["previous_response_id"],
        original["previous_response_id"]
    );
}
