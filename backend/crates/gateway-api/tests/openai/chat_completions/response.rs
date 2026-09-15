use super::*;

#[tokio::test]
async fn empty_primary_usage_details_do_not_hide_aliases() {
    for primary in [
        json!(null),
        json!({}),
        json!({"cached_tokens":null,"reasoning_tokens":null}),
    ] {
        let usage = json!({"input_tokens":8,"output_tokens":2,"input_tokens_details":primary,"output_tokens_details":primary,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":1}});
        let facts =
            gateway_protocol::openai::events::extract_usage(&json!({"usage":usage})).unwrap();
        let (status, value) = json_response(vec![wire(
            "response.done",
            json!({"response":{"status":"completed","output":[message("ok")],"usage":usage}}),
        )])
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value["usage"]["prompt_tokens_details"]["cached_tokens"],
            facts.cached_tokens
        );
        assert_eq!(
            value["usage"]["completion_tokens_details"]["reasoning_tokens"],
            facts.reasoning_tokens
        );
    }
}

#[tokio::test]
async fn partial_reordered_terminal_tools_preserve_identity_and_empty_argument_snapshots() {
    let events = vec![
        wire(
            "response.output_item.added",
            json!({"output_index":2,"item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"a","arguments":""}}),
        ),
        wire(
            "response.function_call_arguments.delta",
            json!({"output_index":2,"delta":"{\"agent\":1}"}),
        ),
        wire(
            "response.output_item.added",
            json!({"output_index":4,"item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"b"}}),
        ),
        wire(
            "response.function_call_arguments.done",
            json!({"output_index":4,"arguments":"[]"}),
        ),
        completed(json!([
            {"type":"function_call","id":"fc_b","arguments":""},
            {"type":"function_call","call_id":"call_a","name":"a"}
        ])),
    ];
    let (status, value) = json_response(events).await;
    assert_eq!(status, StatusCode::OK);
    let tools = value["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["id"], "call_a");
    assert_eq!(tools[0]["function"]["arguments"], "{\"agent\":1}");
    assert_eq!(tools[1]["id"], "call_b");
    assert_eq!(tools[1]["function"]["arguments"], "[]");
}

#[tokio::test]
async fn public_reasoning_summary_deduplicates_done_and_terminal_without_raw_reasoning() {
    let events = vec![
        wire(
            "response.output_item.added",
            json!({"output_index":1,"item":{"id":"r_1","type":"reasoning","summary":[]}}),
        ),
        wire(
            "response.reasoning_text.delta",
            json!({"output_index":1,"content_index":0,"delta":"secret raw"}),
        ),
        wire(
            "response.reasoning_summary_text.delta",
            json!({"output_index":1,"summary_index":0,"delta":"public "}),
        ),
        wire(
            "response.reasoning_summary_text.done",
            json!({"output_index":1,"summary_index":0,"text":"public summary"}),
        ),
        completed(
            json!([{"id":"r_1","type":"reasoning","encrypted_content":"secret encrypted","summary":[{"type":"summary_text","text":"public summary"}]}]),
        ),
    ];
    let (status, value) = json_response(events).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value["choices"][0]["message"]["reasoning_content"],
        "public summary"
    );
    assert!(!value.to_string().contains("secret"));
}

#[tokio::test]
async fn usage_aliases_top_level_and_earlier_snapshot_share_terminal_precedence() {
    for earlier in [false, true] {
        let mut events = Vec::new();
        let usage =
            json!({"prompt_tokens":8,"completion_tokens":2,"cached_tokens":3,"reasoning_tokens":1});
        if earlier {
            events.push(wire("response.in_progress", json!({"usage":usage})));
        }
        let mut terminal = json!({"response":{"status":"completed","output":[message("ok")]}});
        if !earlier {
            terminal["usage"] = usage;
        }
        events.push(wire("response.done", terminal));
        let (status, value) = json_response(events).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value["usage"],
            json!({"prompt_tokens":8,"completion_tokens":2,"total_tokens":10,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":1}})
        );
    }
}

#[tokio::test]
async fn json_terminal_projects_text_refusal_tools_and_usage_without_private_reasoning() {
    let output = json!([
        {"type":"reasoning","encrypted_content":"private-encrypted","summary":[{"text":"private-reasoning"}]},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"},{"type":"refusal","refusal":"declined"}]},
        {"type":"function_call","call_id":"call_a","name":"lookup","arguments":"{\"city\":\"北京\"}"}
    ]);
    let (status, value) = json_response(vec![completed(output)]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["object"], "chat.completion");
    assert_eq!(value["id"], "chatcmpl-req_chat_test");
    assert_eq!(value["created"], 1_700_000_000_u64);
    assert_eq!(value["model"], "public-chat-model");
    assert_eq!(value["choices"][0]["message"]["content"], "answer");
    assert_eq!(value["choices"][0]["message"]["refusal"], "declined");
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"北京\"}"
    );
    assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        value["usage"],
        json!({"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":2}})
    );
    assert!(!value.to_string().contains("private-"));
}

#[tokio::test]
async fn delta_done_and_terminal_snapshots_do_not_duplicate_text() {
    let events = vec![
        delta("你"),
        delta("好"),
        wire(
            "response.output_text.done",
            json!({"output_index":0,"content_index":0,"text":"你好!"}),
        ),
        completed(json!([message("你好!")])),
    ];
    let (status, value) = json_response(events).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["choices"][0]["message"]["content"], "你好!");
}

#[tokio::test]
async fn done_only_and_missing_terminal_output_retain_observed_text() {
    let (status, value) = json_response(vec![
        wire(
            "response.output_text.done",
            json!({"output_index":0,"content_index":0,"text":"done-only"}),
        ),
        wire(
            "response.completed",
            json!({"response":{"status":"completed"}}),
        ),
    ])
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["choices"][0]["message"]["content"], "done-only");
    assert!(value.get("usage").is_none());
}

#[tokio::test]
async fn incomplete_reasons_are_explicit() {
    for (reason, expected) in [
        ("max_output_tokens", "length"),
        ("content_filter", "content_filter"),
    ] {
        let (status, value) = json_response(vec![wire("response.incomplete", json!({"response":{"status":"incomplete","output":[message("partial")],"incomplete_details":{"reason":reason}}}))]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["choices"][0]["finish_reason"], expected);
    }
    let (status, _) = json_response(vec![wire("response.incomplete", json!({"response":{"status":"incomplete","output":[],"incomplete_details":{"reason":"unknown"}}}))]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn malformed_terminal_and_conflicting_snapshots_fail_before_json_commit() {
    for events in [
        vec![delta("already"), completed(json!([message("different")]))],
        vec![wire(
            "response.completed",
            json!({"response":{"status":"failed","output":[]}}),
        )],
        vec![wire(
            "response.completed",
            json!({"response":{"status":"completed","output":null}}),
        )],
        vec![completed(
            json!([{"type":"function_call","call_id":"c","name":"f","arguments":42}]),
        )],
        vec![completed(
            json!([{"type":"message","role":"user","content":[]}]),
        )],
        vec![wire(
            "response.completed",
            json!({"response":{"status":"completed","output":[],"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":99}}}),
        )],
    ] {
        let session = Session::json(events);
        let trace = session.trace.clone();
        let response = call(session, false, false).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !trace
                .events()
                .iter()
                .any(|event| event.starts_with("commit:"))
        );
        assert!(trace.events().contains(&"cancel".to_owned()));
        assert_eq!(
            trace
                .events()
                .iter()
                .filter(|event| *event == "finalize")
                .count(),
            1
        );
    }
}
