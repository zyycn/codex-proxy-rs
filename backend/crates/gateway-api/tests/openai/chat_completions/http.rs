use super::*;

#[tokio::test]
async fn summary_sse_and_absent_usage_do_not_expose_raw_reasoning_or_null_usage_only_chunk() {
    let (_, body) = stream_body(vec![
        Step::Batch(vec![wire("response.reasoning_summary_text.delta", json!({"output_index":0,"summary_index":0,"delta":"summary"}))], CommitRequirement::CommitBeforeDelivery),
        Step::Batch(vec![wire("response.reasoning_text.delta", json!({"delta":"hidden raw"})), wire("response.done", json!({"response":{"status":"completed","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"summary!"}],"encrypted_content":"hidden encrypted"}]}}))], CommitRequirement::AlreadyCommitted),
        Step::Finish,
    ], true).await;
    let values = stream_values(&body);
    let summary: String = values
        .iter()
        .filter_map(|value| {
            value
                .pointer("/choices/0/delta/reasoning_content")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(summary, "summary!");
    assert!(!body.contains("hidden"));
    assert!(values.iter().all(|value| value["choices"] != json!([])));
    assert_eq!(
        values.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
}

async fn legacy_call(session: Session, streaming: bool) -> axum::response::Response {
    let app = crate::openai::api_router(Arc::new(Execution {
        session: Mutex::new(Some(session)),
    }))
    .await;
    app.oneshot(Request::post("/v1/chat/completions").header("authorization", "Bearer sk_chat_test")
        .body(Body::from(json!({"model":"public-chat-model","messages":[{"role":"user","content":"call"}],"functions":[{"name":"lookup","parameters":{"type":"object"}}],"stream":streaming}).to_string())).unwrap()).await.unwrap()
}

#[tokio::test]
async fn legacy_json_and_stream_use_function_call_without_modern_tool_calls() {
    let added = wire(
        "response.output_item.added",
        json!({"output_index":2,"item":{"type":"function_call","call_id":"call_a","name":"lookup","arguments":""}}),
    );
    let args = wire(
        "response.function_call_arguments.delta",
        json!({"output_index":2,"delta":"{raw"}),
    );
    let terminal = wire(
        "response.done",
        json!({"response":{"status":"completed","output":[]}}),
    );
    let response = legacy_call(
        Session::json(vec![added.clone(), args.clone(), terminal.clone()]),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        value["choices"][0]["message"]["function_call"],
        json!({"name":"lookup","arguments":"{raw"})
    );
    assert_eq!(value["choices"][0]["finish_reason"], "function_call");
    assert!(value["choices"][0]["message"].get("tool_calls").is_none());
    let response = legacy_call(
        Session::stream(vec![
            Step::Batch(vec![added], CommitRequirement::CommitBeforeDelivery),
            Step::Batch(vec![args, terminal], CommitRequirement::AlreadyCommitted),
            Step::Finish,
        ]),
        true,
    )
    .await;
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let values = stream_values(&body);
    let calls: Vec<_> = values
        .iter()
        .filter_map(|value| value.pointer("/choices/0/delta/function_call"))
        .collect();
    assert_eq!(calls[0]["name"], "lookup");
    assert_eq!(calls[1], &json!({"arguments":"{raw"}));
    assert_eq!(
        values.last().unwrap()["choices"][0]["finish_reason"],
        "function_call"
    );
    assert!(!body.contains("tool_calls"));
}

#[tokio::test]
async fn legacy_multiple_calls_fail_instead_of_dropping_one() {
    let response = legacy_call(
        Session::json(vec![completed(json!([
            {"type":"function_call","call_id":"a","name":"lookup","arguments":"{}"},
            {"type":"function_call","call_id":"b","name":"lookup","arguments":"{}"}
        ]))]),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

async fn stream_body(steps: Vec<Step>, include_usage: bool) -> (Arc<Trace>, String) {
    let session = Session::stream(steps);
    let trace = session.trace.clone();
    let response = call(session, true, include_usage).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (trace, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn sse_uses_stable_chat_envelopes_and_usage_after_finish() {
    let (trace, body) = stream_body(
        vec![
            Step::Batch(
                vec![wire(
                    "response.created",
                    json!({"response":{"id":"private-id"}}),
                )],
                CommitRequirement::CommitBeforeDelivery,
            ),
            Step::Batch(vec![delta("hello")], CommitRequirement::AlreadyCommitted),
            Step::Batch(
                vec![completed(json!([message("hello!")]))],
                CommitRequirement::AlreadyCommitted,
            ),
            Step::Finish,
        ],
        true,
    )
    .await;
    assert!(!body.contains("event:"));
    assert!(!body.contains("private-id"));
    assert_eq!(body.matches("data: [DONE]\n\n").count(), 1);
    let values = stream_values(&body);
    assert_eq!(
        values[0]["choices"][0]["delta"],
        json!({"role":"assistant","content":""})
    );
    for value in &values {
        assert_eq!(value["id"], "chatcmpl-req_chat_test");
        assert_eq!(value["created"], 1_700_000_000_u64);
        assert_eq!(value["model"], "public-chat-model");
        assert_eq!(value["object"], "chat.completion.chunk");
    }
    let text: String = values
        .iter()
        .filter_map(|value| {
            value
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(text, "hello!");
    assert_eq!(
        values[values.len() - 2]["choices"][0]["finish_reason"],
        "stop"
    );
    assert_eq!(values.last().unwrap()["choices"], json!([]));
    assert_eq!(values.last().unwrap()["usage"]["total_tokens"], 14);
    assert!(
        values[..values.len() - 1]
            .iter()
            .all(|value| value["usage"].is_null())
    );
    assert_eq!(
        trace.events(),
        [
            "next",
            "commit:Some(200)",
            "next",
            "next",
            "next",
            "finalize"
        ]
    );
}

#[tokio::test]
async fn tool_indices_follow_call_order_and_arguments_remain_raw_fragments() {
    let tool = |index, id, name, arguments| {
        wire(
            "response.output_item.added",
            json!({"output_index":index,"item":{"type":"function_call","call_id":id,"name":name,"arguments":arguments}}),
        )
    };
    let (_, body) = stream_body(vec![
        Step::Batch(vec![wire("response.output_item.added", json!({"output_index":0,"item":{"type":"reasoning","encrypted_content":"hidden"}})), tool(2, "call_a", "a", "")], CommitRequirement::CommitBeforeDelivery),
        Step::Batch(vec![wire("response.function_call_arguments.delta", json!({"output_index":2,"delta":"{bad"})), tool(4, "call_b", "b", "[]")], CommitRequirement::AlreadyCommitted),
        Step::Batch(vec![wire("response.completed", json!({"response":{"status":"completed"}}))], CommitRequirement::AlreadyCommitted),
        Step::Finish,
    ], false).await;
    let values = stream_values(&body);
    let calls: Vec<_> = values
        .iter()
        .filter_map(|value| value.pointer("/choices/0/delta/tool_calls/0"))
        .collect();
    assert_eq!(calls[0]["index"], 0);
    assert_eq!(calls[0]["id"], "call_a");
    assert_eq!(
        calls[1],
        &json!({"index":0,"function":{"arguments":"{bad"}})
    );
    assert_eq!(calls[2]["index"], 1);
    assert_eq!(calls[2]["id"], "call_b");
    assert_eq!(
        values.last().unwrap()["choices"][0]["finish_reason"],
        "tool_calls"
    );
    assert!(values.iter().all(|value| value.get("usage").is_none()));
    assert!(!body.contains("hidden"));
}

#[tokio::test]
async fn first_batch_terminal_does_not_leak_success_when_finalization_fails() {
    let (trace, body) = stream_body(
        vec![
            Step::Batch(
                vec![completed(json!([message("withheld")]))],
                CommitRequirement::CommitBeforeDelivery,
            ),
            Step::Fail,
        ],
        true,
    )
    .await;
    let values = stream_values(&body);
    assert!(values.iter().any(|value| value.get("error").is_some()));
    assert!(!body.contains("withheld"));
    assert!(!values.iter().any(|value| {
        value
            .pointer("/choices/0/finish_reason")
            .is_some_and(|reason| !reason.is_null())
    }));
    assert!(!values.iter().any(|value| value["choices"] == json!([])));
    assert_eq!(body.matches("data: [DONE]").count(), 1);
    assert_eq!(
        trace
            .events()
            .iter()
            .filter(|event| *event == "finalize")
            .count(),
        1
    );
}

#[tokio::test]
async fn eof_and_late_encoding_failure_emit_chat_errors_once() {
    for steps in [
        vec![
            Step::Batch(
                vec![delta("partial")],
                CommitRequirement::CommitBeforeDelivery,
            ),
            Step::Finish,
        ],
        vec![
            Step::Batch(
                vec![delta("partial")],
                CommitRequirement::CommitBeforeDelivery,
            ),
            Step::Batch(
                vec![completed(json!([message("conflict")]))],
                CommitRequirement::AlreadyCommitted,
            ),
            Step::Finish,
        ],
    ] {
        let (trace, body) = stream_body(steps, false).await;
        assert_eq!(
            stream_values(&body)
                .iter()
                .filter(|value| value.get("error").is_some())
                .count(),
            1
        );
        assert_eq!(body.matches("data: [DONE]").count(), 1);
        assert!(!body.contains("response.failed"));
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

#[tokio::test]
async fn provider_wire_error_preserves_sdk_error_fields_without_duplicate() {
    let (_, body) = stream_body(vec![
        Step::Batch(vec![delta("partial")], CommitRequirement::CommitBeforeDelivery),
        Step::Batch(vec![wire("error", json!({"error":{"type":"rate_limit_error","code":"rate_limit_exceeded","message":"Synthetic quota limit"}}))], CommitRequirement::AlreadyCommitted),
        Step::Fail,
    ], false).await;
    let values = stream_values(&body);
    let errors: Vec<_> = values
        .iter()
        .filter_map(|value| value.get("error"))
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "rate_limit_exceeded");
    assert_eq!(errors[0]["message"], "Synthetic quota limit");
    assert_eq!(body.matches("data: [DONE]").count(), 1);
}

#[tokio::test]
async fn body_drop_cancels_and_finalizes_once_without_reading_remaining_output() {
    let session = Session::stream(vec![
        Step::Batch(
            vec![delta("first")],
            CommitRequirement::CommitBeforeDelivery,
        ),
        Step::Finish,
    ]);
    let trace = session.trace.clone();
    let response = call(session, true, false).await;
    assert_eq!(trace.events(), ["next", "commit:Some(200)"]);
    drop(response);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        trace
            .events()
            .iter()
            .filter(|event| *event == "cancel")
            .count(),
        1
    );
    assert_eq!(
        trace
            .events()
            .iter()
            .filter(|event| *event == "finalize")
            .count(),
        1
    );
}

#[tokio::test]
async fn unprojectable_canonical_terminal_fails_before_success_finalization() {
    let terminal = ProviderEvent::canonical(gateway_core::event::GatewayEvent::Completed(
        gateway_core::event::ResponseMeta::new("private", "private"),
    ));
    let session = Session::stream(vec![
        Step::Batch(vec![terminal], CommitRequirement::CommitBeforeDelivery),
        Step::Finish,
    ]);
    let trace = session.trace.clone();
    let response = call(session, true, false).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(trace.events().contains(&"cancel".to_owned()));
    assert!(
        !trace
            .events()
            .iter()
            .any(|event| event.starts_with("commit:"))
    );
}

#[tokio::test]
async fn canonical_and_wire_terminal_can_be_separate_events_in_one_batch() {
    let terminal = ProviderEvent::canonical(gateway_core::event::GatewayEvent::Completed(
        gateway_core::event::ResponseMeta::new("private", "private"),
    ));
    let (_, body) = stream_body(
        vec![
            Step::Batch(
                vec![terminal, completed(json!([message("split terminal")]))],
                CommitRequirement::CommitBeforeDelivery,
            ),
            Step::Finish,
        ],
        false,
    )
    .await;
    let values = stream_values(&body);
    assert!(!values.iter().any(|value| value.get("error").is_some()));
    assert_eq!(
        values.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
    assert!(body.contains("split terminal"));
}

#[tokio::test]
async fn later_canonical_only_terminal_cancels_before_next_finalize() {
    let terminal = ProviderEvent::canonical(gateway_core::event::GatewayEvent::Completed(
        gateway_core::event::ResponseMeta::new("private", "private"),
    ));
    let (trace, body) = stream_body(
        vec![
            Step::Batch(
                vec![delta("partial")],
                CommitRequirement::CommitBeforeDelivery,
            ),
            Step::Batch(vec![terminal], CommitRequirement::AlreadyCommitted),
            Step::Finish,
        ],
        true,
    )
    .await;
    assert_eq!(
        trace.events(),
        [
            "next",
            "commit:Some(200)",
            "next",
            "cancel",
            "next",
            "finalize"
        ]
    );
    let values = stream_values(&body);
    assert_eq!(
        values
            .iter()
            .filter(|value| value.get("error").is_some())
            .count(),
        1
    );
    assert!(!values.iter().any(|value| value["choices"] == json!([])));
    assert!(!values.iter().any(|value| {
        value
            .pointer("/choices/0/finish_reason")
            .is_some_and(|reason| !reason.is_null())
    }));
}
#[tokio::test]
async fn client_agent_extension_preserves_successful_chat_response() {
    for with_agent in [false, true] {
        let session = Session::json(vec![completed(json!([message("answer")]))]);
        let app = crate::openai::api_router(Arc::new(Execution {
            session: Mutex::new(Some(session)),
        }))
        .await;
        let mut body = json!({"model":"public-chat-model","messages":[
            {"role":"system","content":"help the user"},
            {"role":"user","content":"hello"}
        ]});
        if with_agent {
            body["messages"][1]["agent"] = json!("craft");
        }
        let response = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("authorization", "Bearer sk_chat_test")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(body["choices"][0]["message"]["content"], "answer");
    }
}
