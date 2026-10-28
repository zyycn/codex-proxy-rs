//! Chat JSON 与 SSE 的真实 wire 投影、去重和协议扩展边界

use axum::{body::to_bytes, http::StatusCode, response::Response};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{
    chat_request, completed, created,
    http::{Fixture, Session, json_body},
    message, wire,
};

pub(super) async fn sse_body(response: Response) -> (String, Vec<Value>) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(text.matches("data: [DONE]").count(), 1, "{text}");
    assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
    assert!(!text.contains("event: response."), "{text}");
    let chunks = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    (text, chunks)
}

fn combined(chunks: &[Value], name: &str) -> String {
    chunks
        .iter()
        .filter_map(|value| value["choices"][0]["delta"][name].as_str())
        .collect()
}

fn tool(id: &str, call_id: &str, name: &str, arguments: &str) -> Value {
    json!({"type":"function_call","id":id,"call_id":call_id,"name":name,"arguments":arguments,"status":"completed"})
}

#[tokio::test]
async fn json_projects_public_identity_text_and_actual_usage() {
    let fixture = Fixture::new(Session::new(vec![completed(json!([message("你好 🌍")]))])).await;
    let response = fixture.post(chat_request(false)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-gateway-request-id"],
        "req_chat_fixture"
    );
    let body = json_body(response).await;
    assert_eq!(body["object"], "chat.completion");
    assert!(body["id"].as_str().unwrap().starts_with("chatcmpl-"));
    assert!(body["created"].as_u64().is_some());
    assert_eq!(body["model"], "model-a");
    assert_eq!(body["choices"][0]["message"]["content"], "你好 🌍");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        body["usage"],
        json!({"prompt_tokens":11,"completion_tokens":7,"total_tokens":18,
        "prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":2}})
    );
}

#[tokio::test]
async fn json_tool_arguments_and_refusal_do_not_become_assistant_text() {
    for output in [
        json!([tool("fc1", "call1", "weather", "{ \"city\" : \"北京\" }")]),
        json!([{"type":"reasoning","id":"rs1","summary":[{"type":"summary_text","text":"private"}]},
            {"type":"message","id":"refusal1","role":"assistant","status":"completed",
             "content":[{"type":"refusal","refusal":"cannot help"}]}]),
    ] {
        let fixture = Fixture::new(Session::new(vec![completed(output.clone())])).await;
        let response = fixture.post(chat_request(false)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        let choice = &body["choices"][0];
        assert_eq!(choice["message"]["content"], Value::Null);
        assert!(!body.to_string().contains("private"));
        if output[0]["type"] == "function_call" {
            assert_eq!(choice["finish_reason"], "tool_calls");
            assert_eq!(
                choice["message"]["tool_calls"][0]["function"]["arguments"],
                "{ \"city\" : \"北京\" }"
            );
            assert_eq!(choice["message"]["tool_calls"][0]["id"], "call1");
        } else {
            assert_eq!(choice["message"]["refusal"], "cannot help");
        }
    }
}

#[tokio::test]
async fn sse_deduplicates_text_snapshots_and_emits_usage_only_when_requested() {
    for include_usage in [false, true] {
        let fixture = Fixture::new(Session::new(vec![created(),
            wire("response.output_text.delta",json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"你好 "})),
            wire("response.output_text.done",json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"text":"你好 🌍"})),
            wire("response.content_part.done",json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"part":{"type":"output_text","text":"你好 🌍"}})),
            wire("response.output_item.done",json!({"output_index":0,"item":message("你好 🌍")})),
            completed(json!([message("你好 🌍")]))])).await;
        let mut body = chat_request(true);
        body["stream_options"] = json!({"include_usage":include_usage});
        let (_, chunks) = sse_body(fixture.post(body).await).await;
        assert_eq!(combined(&chunks, "content"), "你好 🌍");
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| chunk["choices"][0]["delta"]["role"] == "assistant")
                .count(),
            1
        );
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| chunk["choices"][0]["finish_reason"] == "stop")
                .count(),
            1
        );
        for chunk in &chunks {
            assert_eq!(chunk["object"], "chat.completion.chunk");
            assert_eq!(chunk["id"], chunks[0]["id"]);
            assert_eq!(chunk["created"], chunks[0]["created"]);
            assert_eq!(chunk["model"], "model-a");
        }
        let usage: Vec<_> = chunks
            .iter()
            .filter(|chunk| chunk["choices"] == json!([]))
            .collect();
        assert_eq!(usage.len(), usize::from(include_usage));
        if include_usage {
            assert_eq!(usage[0]["usage"]["total_tokens"], 18);
        }
        let captured = fixture.captured();
        assert!(captured.body.get("stream_options").is_none());
    }
}

#[tokio::test]
async fn sse_preserves_interleaved_tools_and_reordered_terminal_snapshots() {
    let first = tool(
        "fc_first",
        "call_first",
        "weather",
        "{ \"city\": \"北京\" }",
    );
    let second = tool("fc_second", "call_second", "search", "{\"query\":\"Rust\"}");
    let fixture = Fixture::new(Session::new(vec![
        created(),
        wire(
            "response.output_item.added",
            json!({"output_index":2,"item":tool("fc_first","call_first","weather","")}),
        ),
        wire(
            "response.output_item.added",
            json!({"output_index":5,"item":tool("fc_second","call_second","search","")}),
        ),
        wire(
            "response.function_call_arguments.delta",
            json!({"output_index":2,"item_id":"fc_first","delta":"{ \"city\": "}),
        ),
        wire(
            "response.function_call_arguments.delta",
            json!({"output_index":5,"item_id":"fc_second","delta":"{\"query\":"}),
        ),
        wire(
            "response.function_call_arguments.delta",
            json!({"output_index":2,"item_id":"fc_first","delta":"\"北京\" }"}),
        ),
        wire(
            "response.function_call_arguments.done",
            json!({"output_index":5,"item_id":"fc_second","arguments":second["arguments"]}),
        ),
        wire(
            "response.output_item.done",
            json!({"output_index":2,"item":first}),
        ),
        completed(json!([second, first])),
    ]))
    .await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    let mut tools = BTreeMap::<u64, (String, String, String)>::new();
    for chunk in &chunks {
        if let Some(calls) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
            for call in calls {
                let entry = tools.entry(call["index"].as_u64().unwrap()).or_default();
                if let Some(id) = call["id"].as_str() {
                    entry.0.push_str(id);
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    entry.1.push_str(name);
                }
                if let Some(arguments) = call["function"]["arguments"].as_str() {
                    entry.2.push_str(arguments);
                }
            }
        }
    }
    assert_eq!(
        tools,
        BTreeMap::from([
            (
                0,
                (
                    "call_first".into(),
                    "weather".into(),
                    "{ \"city\": \"北京\" }".into()
                )
            ),
            (
                1,
                (
                    "call_second".into(),
                    "search".into(),
                    "{\"query\":\"Rust\"}".into()
                )
            ),
        ])
    );
    assert_eq!(
        chunks.last().unwrap()["choices"][0]["finish_reason"],
        "tool_calls"
    );
}

#[tokio::test]
async fn extra_attributes_and_auxiliary_events_preserve_existing_chat_results() {
    let mut text = message("preserved");
    text["future_optional"] = json!({"opaque":true});
    text["content"][0]["future_attribute"] = json!([1, 2, 3]);
    let fixture = Fixture::new(Session::new(vec![
        wire("response.future_auxiliary", json!({"future":true})),
        wire(
            "response.reasoning_summary_text.delta",
            json!({"output_index":0,"summary_index":0,"delta":"private"}),
        ),
        created(),
        completed(json!([text])),
    ]))
    .await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    assert_eq!(combined(&chunks, "content"), "preserved");
    assert!(
        !chunks
            .iter()
            .any(|chunk| chunk.to_string().contains("private"))
    );
    assert_eq!(
        fixture
            .trace
            .discarded
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
}

#[tokio::test]
async fn incomplete_projects_only_supported_finish_reasons() {
    for (reason, expected) in [
        ("max_output_tokens", "length"),
        ("content_filter", "content_filter"),
    ] {
        for streaming in [false, true] {
            let event = wire(
                "response.incomplete",
                json!({"response":{
                "id":"resp_chat_test","status":"incomplete","output":[message("partial")],
                "incomplete_details":{"reason":reason}}}),
            );
            let fixture = Fixture::new(Session::new(vec![created(), event])).await;
            let response = fixture.post(chat_request(streaming)).await;
            if streaming {
                let (_, chunks) = sse_body(response).await;
                assert_eq!(
                    chunks.last().unwrap()["choices"][0]["finish_reason"],
                    expected
                );
            } else {
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(
                    json_body(response).await["choices"][0]["finish_reason"],
                    expected
                );
            }
        }
    }
}

#[tokio::test]
async fn encrypted_reasoning_stays_private_while_public_text_and_usage_are_projected() {
    let reasoning = json!({
        "id":"rs_private","type":"reasoning",
        "summary":[{"type":"summary_text","text":"PRIVATE_SUMMARY_MARKER"}],
        "encrypted_content":"OPAQUE_ENCRYPTED_REASONING_MARKER"
    });
    for streaming in [false, true] {
        let fixture = Fixture::new(Session::new(vec![
            created(),
            wire(
                "response.output_item.added",
                json!({"output_index":0,"item":{"id":"rs_private","type":"reasoning","summary":[]}}),
            ),
            wire(
                "response.reasoning_summary_text.delta",
                json!({"item_id":"rs_private","output_index":0,"summary_index":0,"delta":"PRIVATE_SUMMARY_MARKER"}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":0,"item":reasoning}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":1,"item":message("public answer")}),
            ),
            completed(json!([reasoning, message("public answer")])),
        ]))
        .await;
        let mut request = chat_request(streaming);
        if streaming {
            request["stream_options"] = json!({"include_usage":true});
        }
        let response = fixture.post(request).await;
        let (body, content, usage) = if streaming {
            let (body, chunks) = sse_body(response).await;
            let usage = chunks
                .iter()
                .find(|chunk| chunk["choices"] == json!([]))
                .expect("terminal usage chunk")["usage"]
                .clone();
            (body, combined(&chunks, "content"), usage)
        } else {
            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            (
                body.to_string(),
                body["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                body["usage"].clone(),
            )
        };
        assert_eq!(content, "public answer");
        assert!(!body.contains("PRIVATE_SUMMARY_MARKER"), "{body}");
        assert!(
            !body.contains("OPAQUE_ENCRYPTED_REASONING_MARKER"),
            "{body}"
        );
        assert_eq!(usage["completion_tokens_details"]["reasoning_tokens"], 2);
    }
}

#[tokio::test]
async fn unrepresentable_business_output_fails_without_successful_chat_completion() {
    for output in [
        json!({"id":"future_output","type":"future_business_output","payload":"unrepresentable"}),
        json!({"id":"ig_native","type":"image_generation_call","status":"completed","result":"OPAQUE_IMAGE_MARKER"}),
    ] {
        for streaming in [false, true] {
            let fixture =
                Fixture::new(Session::new(vec![created(), completed(json!([output]))])).await;
            let mut request = chat_request(streaming);
            if streaming {
                request["stream_options"] = json!({"include_usage":true});
            }
            let response = fixture.post(request).await;
            if streaming {
                let (body, chunks) = sse_body(response).await;
                assert!(chunks.iter().any(|chunk| chunk.get("error").is_some()));
                assert!(
                    !chunks
                        .iter()
                        .any(|chunk| chunk["choices"][0]["finish_reason"].is_string())
                );
                assert!(!chunks.iter().any(|chunk| chunk["choices"] == json!([])));
                assert!(!body.contains("OPAQUE_IMAGE_MARKER"));
            } else {
                assert!(response.status().is_server_error());
                let body = json_body(response).await;
                assert!(body.get("error").is_some());
                assert!(body.get("choices").is_none());
                assert!(!body.to_string().contains("OPAQUE_IMAGE_MARKER"));
            }
        }
    }
}

#[tokio::test]
async fn stream_without_terminal_does_not_emit_a_successful_finish_or_usage() {
    let fixture = Fixture::new(Session::new(vec![
        created(),
        wire(
            "response.output_text.delta",
            json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"partial"}),
        ),
    ]))
    .await;
    let mut body = chat_request(true);
    body["stream_options"] = json!({"include_usage":true});
    let (_, chunks) = sse_body(fixture.post(body).await).await;
    assert!(chunks.iter().any(|chunk| chunk.get("error").is_some()));
    assert!(
        !chunks
            .iter()
            .any(|chunk| chunk["choices"][0]["finish_reason"].is_string())
    );
    assert!(!chunks.iter().any(|chunk| chunk["choices"] == json!([])));
}

#[tokio::test]
async fn first_batch_terminal_can_finish_without_creating_usage_facts() {
    let fixture = Fixture::new(Session::new(vec![wire(
        "response.completed",
        json!({"response":{
            "id":"resp_chat_test","status":"completed","output":[message("done only")]
        }}),
    )]))
    .await;
    let mut body = chat_request(true);
    body["stream_options"] = json!({"include_usage":true});
    let (_, chunks) = sse_body(fixture.post(body).await).await;
    assert_eq!(combined(&chunks, "content"), "done only");
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk["choices"][0]["finish_reason"] == "stop")
            .count(),
        1
    );
    assert!(!chunks.iter().any(|chunk| chunk["usage"].is_object()));
    assert!(
        fixture
            .trace
            .finalized
            .load(std::sync::atomic::Ordering::SeqCst)
    );
}

#[tokio::test]
async fn native_sse_byte_fragments_and_multiple_events_preserve_unicode() {
    let first = gateway_protocol::openai::sse::encode_sse_event("response.output_text.delta",&json!({
        "type":"response.output_text.delta","item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"你"
    }).to_string());
    let second = gateway_protocol::openai::sse::encode_sse_event("response.output_text.delta",&json!({
        "type":"response.output_text.delta","item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"好"
    }).to_string());
    let split = first.find('你').unwrap() + 1;
    let mut remainder = first.as_bytes()[split..].to_vec();
    remainder.extend_from_slice(second.as_bytes());
    let raw = |bytes| {
        gateway_core::event::ProviderEvent::wire(
            gateway_core::event::ProtocolWireEvent::raw_sse("openai", bytes).unwrap(),
        )
    };
    let fixture = Fixture::new(Session::new(vec![
        created(),
        raw(bytes::Bytes::copy_from_slice(&first.as_bytes()[..split])),
        raw(bytes::Bytes::from(remainder)),
        completed(json!([message("你好")])),
    ]))
    .await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    assert_eq!(combined(&chunks, "content"), "你好");
    assert_eq!(
        chunks.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
}

#[tokio::test]
async fn refusal_deltas_and_done_snapshots_produce_only_standard_refusal() {
    let output = json!({"id":"msg_refusal","type":"message","role":"assistant","status":"completed",
        "content":[{"type":"refusal","refusal":"cannot help"}]});
    let fixture = Fixture::new(Session::new(vec![created(),
        wire("response.refusal.delta",json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"delta":"cannot "})),
        wire("response.refusal.done",json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"refusal":"cannot help"})),
        completed(json!([output]))])).await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    assert_eq!(combined(&chunks, "refusal"), "cannot help");
    assert_eq!(combined(&chunks, "content"), "");
    assert_eq!(
        chunks.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
}

#[tokio::test]
async fn auxiliary_events_after_completed_preserve_exactly_one_finish_and_usage() {
    for with_created in [false, true] {
        // 终态与辅助事件同属一批 Core 交付 不构造已终结会话继续产出事件的另一种异常
        let session = Session::new(if with_created {
            vec![created()]
        } else {
            Vec::new()
        })
        .with_batch(vec![
            completed(json!([message("completed text")])),
            wire(
                "response.future_metadata",
                json!({"response_id":"resp_chat_test","future":{"kept":true}}),
            ),
        ]);
        let fixture = Fixture::new(session).await;
        let mut body = chat_request(true);
        body["stream_options"] = json!({"include_usage":true});
        let (_, chunks) = sse_body(fixture.post(body).await).await;
        assert_eq!(combined(&chunks, "content"), "completed text");
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| chunk["choices"][0]["finish_reason"] == "stop")
                .count(),
            1
        );
        let usage = chunks
            .iter()
            .filter(|chunk| chunk["choices"] == json!([]))
            .collect::<Vec<_>>();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0]["usage"]["total_tokens"], 18);
        assert!(!chunks.iter().any(|chunk| chunk.get("error").is_some()));
        assert_eq!(
            fixture
                .trace
                .commits
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}

#[tokio::test]
async fn wire_failures_preserve_existing_recovery_fields_without_success_chunks() {
    for shape in ["response.failed", "error", "bare_error"] {
        let mut expected = json!({
            "type":"rate_limit_error","code":"rate_limit_exceeded","param":null,
            "message":"retry later","headers":{"retry-after":"2"},
            "recovery":{"action":"retry","future":[1,2]},
            "future_error_property":{"enabled":true}
        });
        let failure = match shape {
            "response.failed" => wire(
                "response.failed",
                json!({"response":{
                    "id":"resp_chat_test","object":"response","model":"upstream-model",
                    "status":"failed","output":[],"error":expected.clone()
                }}),
            ),
            "error" => wire("error", json!({"error":expected.clone()})),
            _ => {
                // 裸事件的 type 是 wire 名称，仅该包装身份归一化为 Chat 错误类型
                let failure = wire("error", expected.clone());
                expected["type"] = json!("server_error");
                failure
            }
        };
        let fixture = Fixture::new(Session::new(vec![created(), failure])).await;
        let mut body = chat_request(true);
        body["stream_options"] = json!({"include_usage":true});
        let (_, chunks) = sse_body(fixture.post(body).await).await;
        let errors = chunks
            .iter()
            .filter_map(|chunk| chunk.get("error"))
            .collect::<Vec<_>>();
        assert_eq!(errors, vec![&expected], "{shape}");
        assert!(
            !chunks
                .iter()
                .any(|chunk| chunk["choices"][0]["finish_reason"].is_string()),
            "{shape}"
        );
        assert!(!chunks.iter().any(|chunk| chunk["choices"] == json!([])));
        assert!(!chunks.iter().any(|chunk| chunk["usage"].is_object()));
        assert_eq!(
            fixture
                .trace
                .commits
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}

#[tokio::test]
async fn conflicting_text_snapshot_fails_instead_of_appending_replacement_content() {
    let fixture = Fixture::new(Session::new(vec![created(),
        wire("response.output_text.delta",json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"already sent"})),
        completed(json!([message("different result")]))])).await;
    let (_, chunks) = sse_body(fixture.post(chat_request(true)).await).await;
    assert_eq!(combined(&chunks, "content"), "already sent");
    assert!(chunks.iter().any(|chunk| chunk.get("error").is_some()));
    assert!(
        !chunks
            .iter()
            .any(|chunk| chunk["choices"][0]["finish_reason"].is_string())
    );
}
