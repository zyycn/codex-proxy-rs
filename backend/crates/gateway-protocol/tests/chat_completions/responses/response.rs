//! Responses 到 Chat 的完整响应、SSE增量及历史字段回归

use gateway_protocol::chat_completions::{
    ChatAccumulator, ChatError, ChatIdentity,
    responses::{ChatStreamEncoder, project_response},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct Session(Vec<Value>);
impl Session {
    fn new(events: Vec<Value>) -> Self {
        Self(events)
    }
    fn with_batch(mut self, events: Vec<Value>) -> Self {
        self.0.extend(events);
        self
    }
}
struct Fixture(Session);
struct Projection {
    json: Option<Value>,
    chunks: Vec<Value>,
    error: Option<ChatError>,
}
impl Fixture {
    fn new(events: Session) -> Self {
        Self(events)
    }
    fn post(&self, request: Value) -> Projection {
        let identity = chat_identity();
        let mut result = Projection {
            json: None,
            chunks: Vec::new(),
            error: None,
        };
        if request["stream"] == true {
            let mut encoder = ChatStreamEncoder::new(
                identity,
                request["stream_options"]["include_usage"] == true,
            );
            for event in &self.0.0 {
                match encoder.push_event(event["type"].as_str(), event) {
                    Ok(chunks) => result.chunks.extend(chunks),
                    Err(error) => {
                        result.error = Some(error);
                        return result;
                    }
                }
            }
            match encoder.finish() {
                Ok(chunks) => result.chunks.extend(chunks),
                Err(error) => result.error = Some(error),
            }
        } else {
            let projected = if self.0.0.len() == 1 {
                self.0.0[0]
                    .get("response")
                    .ok_or(ChatError::IncompleteStream)
                    .and_then(|value| project_response(value, &identity))
            } else {
                let mut encoder = ChatStreamEncoder::new(identity, true);
                let mut accumulator = ChatAccumulator::default();
                let mut collect = || {
                    for event in &self.0.0 {
                        for chunk in encoder.push_event(event["type"].as_str(), event)? {
                            accumulator.push(&chunk)?;
                        }
                    }
                    for chunk in encoder.finish()? {
                        accumulator.push(&chunk)?;
                    }
                    Ok::<_, ChatError>(())
                };
                collect().and_then(|()| accumulator.finish())
            };
            match projected {
                Ok(value) => result.json = Some(value),
                Err(error) => result.error = Some(error),
            }
        }
        result
    }
}
fn json_body(response: Projection) -> Value {
    assert!(response.error.is_none(), "{:?}", response.error);
    response.json.unwrap()
}
fn sse_body(response: Projection) -> (String, Vec<Value>) {
    assert!(response.error.is_none(), "{:?}", response.error);
    (
        serde_json::to_string(&response.chunks).unwrap(),
        response.chunks,
    )
}
fn chat_identity() -> ChatIdentity {
    ChatIdentity {
        id: "chatcmpl-test".to_owned(),
        created: 100,
        model: "model-a".to_owned(),
    }
}
fn chat_request(stream: bool) -> Value {
    json!({"model":"model-a","messages":[{"role":"user","content":"hello"}],"stream":stream})
}
fn wire(kind: &str, mut value: Value) -> Value {
    value["type"] = json!(kind);
    value
}
fn completed(output: Value) -> Value {
    wire(
        "response.completed",
        json!({"response":{"id":"resp_chat_test","object":"response","model":"upstream-model","status":"completed","output":output,"usage":{"input_tokens":11,"output_tokens":7,"total_tokens":18,"input_tokens_details":{"cached_tokens":3},"output_tokens_details":{"reasoning_tokens":2}}}}),
    )
}
fn message(text: &str) -> Value {
    json!({"id":"msg_chat_test","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]})
}
fn created() -> Value {
    wire(
        "response.created",
        json!({"response":{"id":"resp_chat_test","object":"response","model":"upstream-model","status":"in_progress","output":[]}}),
    )
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

#[test]
fn json_projects_public_identity_text_and_actual_usage() {
    let fixture = Fixture::new(Session::new(vec![completed(json!([message("你好 🌍")]))]));
    let response = fixture.post(chat_request(false));
    let body = json_body(response);
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

#[test]
fn web_search_projects_citations_in_json_and_streams_the_original_links() {
    let linked = "[IANA](https://www.iana.org/)";
    let text = format!("见 {linked}");
    let citation = json!({"type":"url_citation","url":"https://www.iana.org/","title":"IANA",
        "start_index":2,"end_index":text.chars().count()});
    let mut prefix = message("🌍 ");
    prefix["id"] = json!("msg_intro");
    let search = json!({"type":"web_search_call","id":"ws_lookup","status":"completed",
        "action":{"type":"search","query":"IANA"}});
    let mut answer = message(&text);
    answer["content"][0]["annotations"] = json!([citation]);
    for stream in [false, true] {
        let fixture = Fixture::new(Session::new(vec![
            created(),
            wire(
                "response.output_item.done",
                json!({"output_index":0,"item":prefix}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":1,"item":search}),
            ),
            wire(
                "response.output_text.delta",
                json!({"output_index":2,"content_index":0,
                "item_id":"msg_chat_test","delta":text}),
            ),
            wire(
                "response.output_text.annotation.added",
                json!({"output_index":2,"content_index":0,
                "item_id":"msg_chat_test","annotation_index":0,"annotation":citation}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":2,"item":answer}),
            ),
            completed(json!([])),
        ]));
        let mut request = chat_request(stream);
        request["web_search_options"] = json!({});
        let response = fixture.post(request);
        let expected = format!("🌍 {text}");
        if stream {
            let (_, chunks) = sse_body(response);
            assert_eq!(combined(&chunks, "content"), expected);
            assert_eq!(
                chunks.last().unwrap()["choices"][0]["finish_reason"],
                "stop"
            );
        } else {
            let body = json_body(response);
            let choice = &body["choices"][0];
            assert_eq!(choice["finish_reason"], "stop");
            assert_eq!(choice["message"]["content"], expected);
            assert_eq!(
                choice["message"]["annotations"],
                json!([{"type":"url_citation","url_citation":{
                "url":"https://www.iana.org/","title":"IANA","start_index":4,"end_index":expected.chars().count()}}])
            );
        }
    }
}

#[test]
fn json_tool_arguments_and_refusal_do_not_become_assistant_text() {
    for output in [
        json!([tool("fc1", "call1", "weather", "{ \"city\" : \"北京\" }")]),
        json!([{"type":"reasoning","id":"rs1","summary":[{"type":"summary_text","text":"public summary"}]},
            {"type":"message","id":"refusal1","role":"assistant","status":"completed",
             "content":[{"type":"refusal","refusal":"cannot help"}]}]),
    ] {
        let fixture = Fixture::new(Session::new(vec![completed(output.clone())]));
        let response = fixture.post(chat_request(false));
        let body = json_body(response);
        let choice = &body["choices"][0];
        assert_eq!(choice["message"]["content"], Value::Null);
        if output[0]["type"] == "reasoning" {
            assert_eq!(choice["message"]["reasoning_content"], "public summary");
        }
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

#[test]
fn sse_deduplicates_text_snapshots_and_emits_usage_only_when_requested() {
    for include_usage in [false, true] {
        let terminal = completed(json!([message("你好 🌍")]));
        let events = [
            created(),
            wire(
                "response.output_text.delta",
                json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"你好 "}),
            ),
            wire(
                "response.output_text.done",
                json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"text":"你好 🌍"}),
            ),
            wire(
                "response.content_part.done",
                json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"part":{"type":"output_text","text":"你好 🌍"}}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":0,"item":message("你好 🌍")}),
            ),
            terminal.clone(),
        ];
        let identity = chat_identity();
        let mut encoder = ChatStreamEncoder::new(identity.clone(), include_usage);
        let mut chunks = Vec::new();
        for event in events {
            chunks.extend(encoder.push_event(None, &event).unwrap());
        }
        assert!(encoder.finish().unwrap().is_empty());
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
            assert_eq!(chunk["id"], identity.id);
            assert_eq!(chunk["created"], identity.created);
            assert_eq!(chunk["model"], identity.model);
        }
        let usage: Vec<_> = chunks
            .iter()
            .filter(|chunk| chunk["choices"] == json!([]))
            .collect();
        assert_eq!(usage.len(), usize::from(include_usage));
        if include_usage {
            assert_eq!(usage[0]["usage"]["total_tokens"], 18);
            let mut accumulator = ChatAccumulator::default();
            for chunk in &chunks {
                accumulator.push(chunk).unwrap();
            }
            assert_eq!(
                accumulator.finish().unwrap(),
                project_response(&terminal["response"], &identity).unwrap()
            );
        }
    }
}

#[test]
fn sse_preserves_interleaved_tools_and_reordered_terminal_snapshots() {
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
    ]));
    let (_, chunks) = sse_body(fixture.post(chat_request(true)));
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

#[test]
fn extra_attributes_and_auxiliary_events_preserve_existing_chat_results() {
    let mut text = message("preserved");
    text["future_optional"] = json!({"opaque":true});
    text["content"][0]["future_attribute"] = json!([1, 2, 3]);
    let fixture = Fixture::new(Session::new(vec![
        wire("response.future_auxiliary", json!({"future":true})),
        wire(
            "response.future_reasoning.delta",
            json!({"output_index":0,"summary_index":0,"delta":"private"}),
        ),
        created(),
        completed(json!([text])),
    ]));
    let (_, chunks) = sse_body(fixture.post(chat_request(true)));
    assert_eq!(combined(&chunks, "content"), "preserved");
    assert!(
        !chunks
            .iter()
            .any(|chunk| chunk.to_string().contains("private"))
    );
}

#[test]
fn compaction_state_preserves_chat_answer_without_exposing_ciphertext() {
    for kind in ["compaction", "compaction_summary", "context_compaction"] {
        for streaming in [false, true] {
            let state =
                json!({"type":kind,"id":"ctx_1","encrypted_content":"opaque-private-state"});
            let fixture = Fixture::new(Session::new(vec![
                created(),
                wire(
                    "response.output_item.added",
                    json!({"output_index":0,"item":state}),
                ),
                wire(
                    "response.output_item.done",
                    json!({"output_index":0,"item":state}),
                ),
                completed(json!([state, message("preserved")])),
            ]));
            let response = fixture.post(chat_request(streaming));
            if streaming {
                let (text, chunks) = sse_body(response);
                assert_eq!(combined(&chunks, "content"), "preserved");
                assert!(!text.contains("opaque-private-state"));
                assert_eq!(
                    chunks.last().unwrap()["choices"][0]["finish_reason"],
                    "stop"
                );
            } else {
                let body = json_body(response);
                assert_eq!(body["choices"][0]["message"]["content"], "preserved");
                assert_eq!(body["choices"][0]["finish_reason"], "stop");
                assert!(!body.to_string().contains("opaque-private-state"));
            }
        }
    }
}

#[test]
fn incomplete_projects_only_supported_finish_reasons() {
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
            let fixture = Fixture::new(Session::new(vec![created(), event]));
            let response = fixture.post(chat_request(streaming));
            if streaming {
                let (_, chunks) = sse_body(response);
                assert_eq!(
                    chunks.last().unwrap()["choices"][0]["finish_reason"],
                    expected
                );
            } else {
                assert_eq!(json_body(response)["choices"][0]["finish_reason"], expected);
            }
        }
    }
}

#[test]
fn encrypted_reasoning_stays_private_while_public_text_and_usage_are_projected() {
    let reasoning = json!({
        "id":"rs_private","type":"reasoning",
        "summary":[{"type":"summary_text","text":"PUBLIC_SUMMARY_MARKER"}],
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
                json!({"item_id":"rs_private","output_index":0,"summary_index":0,"delta":"PUBLIC_SUMMARY_MARKER"}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":0,"item":reasoning}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":1,"item":message("public answer")}),
            ),
            wire(
                "response.completed",
                json!({"response":{
                    "id":"resp_chat_test","object":"response","model":"upstream-model","status":"completed",
                    "output":[reasoning,message("public answer")],
                    "usage":{"input_tokens":11,"output_tokens":7,"total_tokens":18,
                        "input_tokens_details":{"cached_tokens":3,"cache_write_tokens":4},
                        "output_tokens_details":{"reasoning_tokens":2,"accepted_prediction_tokens":1}}
                }}),
            ),
        ]));
        let mut request = chat_request(streaming);
        if streaming {
            request["stream_options"] = json!({"include_usage":true});
        }
        let response = fixture.post(request);
        let (body, content, usage) = if streaming {
            let (body, chunks) = sse_body(response);
            let usage = chunks
                .iter()
                .find(|chunk| chunk["choices"] == json!([]))
                .expect("terminal usage chunk")["usage"]
                .clone();
            (body, combined(&chunks, "content"), usage)
        } else {
            let body = json_body(response);
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
        assert!(body.contains("PUBLIC_SUMMARY_MARKER"), "{body}");
        assert!(
            !body.contains("OPAQUE_ENCRYPTED_REASONING_MARKER"),
            "{body}"
        );
        assert_eq!(usage["completion_tokens_details"]["reasoning_tokens"], 2);
        assert_eq!(
            usage["completion_tokens_details"]["accepted_prediction_tokens"],
            1
        );
        assert_eq!(usage["prompt_tokens_details"]["cache_write_tokens"], 4);
    }
}

#[test]
fn unrepresentable_business_output_fails_without_successful_chat_completion() {
    for output in [
        json!({"id":"future_output","type":"future_business_output","payload":"unrepresentable"}),
        json!({"id":"action_native","type":"computer_call","status":"completed","call_id":"call_action","action":{"type":"click","x":1,"y":2}}),
    ] {
        for streaming in [false, true] {
            let fixture = Fixture::new(Session::new(vec![created(), completed(json!([output]))]));
            let mut request = chat_request(streaming);
            if streaming {
                request["stream_options"] = json!({"include_usage":true});
            }
            let response = fixture.post(request);
            assert_eq!(response.error, Some(ChatError::InvalidResponse));
            assert!(
                !response
                    .chunks
                    .iter()
                    .any(|chunk| chunk["choices"][0]["finish_reason"].is_string())
            );
            assert!(response.json.is_none());
        }
    }
}

#[test]
fn stream_without_terminal_does_not_emit_a_successful_finish_or_usage() {
    let fixture = Fixture::new(Session::new(vec![
        created(),
        wire(
            "response.output_text.delta",
            json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"partial"}),
        ),
    ]));
    let mut body = chat_request(true);
    body["stream_options"] = json!({"include_usage":true});
    let response = fixture.post(body);
    assert_eq!(response.error, Some(ChatError::IncompleteStream));
    assert!(
        !response
            .chunks
            .iter()
            .any(|chunk| chunk["choices"][0]["finish_reason"].is_string())
    );
    assert!(
        !response
            .chunks
            .iter()
            .any(|chunk| chunk["choices"] == json!([]))
    );
}

#[test]
fn first_batch_terminal_can_finish_without_creating_usage_facts() {
    let fixture = Fixture::new(Session::new(vec![wire(
        "response.completed",
        json!({"response":{
            "id":"resp_chat_test","status":"completed","output":[message("done only")]
        }}),
    )]));
    let mut body = chat_request(true);
    body["stream_options"] = json!({"include_usage":true});
    let (_, chunks) = sse_body(fixture.post(body));
    assert_eq!(combined(&chunks, "content"), "done only");
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk["choices"][0]["finish_reason"] == "stop")
            .count(),
        1
    );
    assert!(!chunks.iter().any(|chunk| chunk["usage"].is_object()));
}

#[test]
fn refusal_deltas_and_done_snapshots_produce_only_standard_refusal() {
    let output = json!({"id":"msg_refusal","type":"message","role":"assistant","status":"completed",
        "content":[{"type":"refusal","refusal":"cannot help"}]});
    let fixture = Fixture::new(Session::new(vec![
        created(),
        wire(
            "response.refusal.delta",
            json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"delta":"cannot "}),
        ),
        wire(
            "response.refusal.done",
            json!({"item_id":"msg_refusal","output_index":0,"content_index":0,"refusal":"cannot help"}),
        ),
        completed(json!([output])),
    ]));
    let (_, chunks) = sse_body(fixture.post(chat_request(true)));
    assert_eq!(combined(&chunks, "refusal"), "cannot help");
    assert_eq!(combined(&chunks, "content"), "");
    assert_eq!(
        chunks.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
}

#[test]
fn auxiliary_events_after_completed_preserve_exactly_one_finish_and_usage() {
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
        let fixture = Fixture::new(session);
        let mut body = chat_request(true);
        body["stream_options"] = json!({"include_usage":true});
        let (_, chunks) = sse_body(fixture.post(body));
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
    }
}

#[test]
fn wire_failures_preserve_existing_recovery_fields_without_success_chunks() {
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
        let fixture = Fixture::new(Session::new(vec![created(), failure]));
        let mut body = chat_request(true);
        body["stream_options"] = json!({"include_usage":true});
        let (_, chunks) = sse_body(fixture.post(body));
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
    }
}

#[test]
fn conflicting_text_snapshot_fails_instead_of_appending_replacement_content() {
    let fixture = Fixture::new(Session::new(vec![
        created(),
        wire(
            "response.output_text.delta",
            json!({"item_id":"msg_chat_test","output_index":0,"content_index":0,"delta":"already sent"}),
        ),
        completed(json!([message("different result")])),
    ]));
    let response = fixture.post(chat_request(true));
    assert_eq!(response.error, Some(ChatError::InvalidResponse));
    assert_eq!(combined(&response.chunks, "content"), "already sent");
    assert!(
        !response
            .chunks
            .iter()
            .any(|chunk| chunk["choices"][0]["finish_reason"].is_string())
    );
}

#[test]
fn custom_and_function_calls_keep_distinct_shapes_in_json_and_interleaved_streams() {
    let custom = json!({"id":"ct_1","type":"custom_tool_call","call_id":"call_custom","name":"patch","input":"first\nsecond"});
    let function = json!({"id":"fc_1","type":"function_call","call_id":"call_function","name":"lookup","arguments":"{}"});
    let output = json!([
        custom, function,
        {"type":"reasoning","id":"reason","summary":[{"type":"summary_text","text":"thought"}],"encrypted_content":"PRIVATE_REASONING"},
        {"type":"message","id":"refusal","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"no"}]}
    ]);
    let direct =
        project_response(&completed(output.clone())["response"], &chat_identity()).unwrap();
    let projected = &direct["choices"][0]["message"];
    assert_eq!(projected["refusal"], "no");
    assert_eq!(projected["reasoning_content"], "thought");
    assert_eq!(
        projected["tool_calls"],
        json!([
            {"id":"call_custom","type":"custom","custom":{"name":"patch","input":"first\nsecond"}},
            {"id":"call_function","type":"function","function":{"name":"lookup","arguments":"{}"}}
        ])
    );
    assert!(!direct.to_string().contains("PRIVATE_REASONING"));
    for streaming in [false, true] {
        let mut initial_custom = custom.clone();
        initial_custom["input"] = json!("");
        let mut initial_function = function.clone();
        initial_function["arguments"] = json!("");
        let fixture = Fixture::new(Session::new(vec![
            created(),
            wire(
                "response.output_item.added",
                json!({"output_index":0,"item":initial_custom}),
            ),
            wire(
                "response.output_item.added",
                json!({"output_index":1,"item":initial_function}),
            ),
            wire(
                "response.custom_tool_call_input.delta",
                json!({"output_index":0,"item_id":"ct_1","delta":"first\n"}),
            ),
            wire(
                "response.function_call_arguments.delta",
                json!({"output_index":1,"item_id":"fc_1","delta":"{}"}),
            ),
            wire(
                "response.custom_tool_call_input.done",
                json!({"output_index":0,"item_id":"ct_1","input":"first\nsecond"}),
            ),
            completed(output.clone()),
        ]));
        let response = fixture.post(chat_request(streaming));
        if streaming {
            let (body, chunks) = sse_body(response);
            assert_eq!(combined(&chunks, "refusal"), "no");
            assert_eq!(combined(&chunks, "reasoning_content"), "thought");
            assert!(!body.contains("PRIVATE_REASONING"));
            let calls = chunks
                .iter()
                .filter_map(|chunk| chunk["choices"][0]["delta"]["tool_calls"].as_array())
                .flatten()
                .collect::<Vec<_>>();
            assert_eq!(
                calls.iter().filter(|call| call["type"] == "custom").count(),
                1
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call["type"] == "function")
                    .count(),
                1
            );
            assert_eq!(
                calls
                    .iter()
                    .filter_map(|call| call["custom"]["input"].as_str())
                    .collect::<String>(),
                "first\nsecond"
            );
            assert_eq!(
                calls
                    .iter()
                    .filter_map(|call| call["function"]["arguments"].as_str())
                    .collect::<String>(),
                "{}"
            );
            assert_eq!(
                chunks.last().unwrap()["choices"][0]["finish_reason"],
                "tool_calls"
            );
        } else {
            let body = json_body(response);
            assert_eq!(body, direct);
        }
    }
}

#[test]
fn image_previews_and_final_images_are_projected_once_per_distinct_snapshot() {
    let image = json!({"id":"ig_one","type":"image_generation_call","status":"completed","result":"ZmluYWw=","output_format":"webp"});
    for streaming in [false, true] {
        let fixture = Fixture::new(Session::new(vec![
            created(),
            wire(
                "response.image_generation_call.partial_image",
                json!({"output_index":0,"item_id":"ig_one","partial_image_b64":"cHJldmlldw==","output_format":"webp"}),
            ),
            wire(
                "response.output_item.done",
                json!({"output_index":0,"item":image}),
            ),
            completed(json!([image, message("created")])),
        ]));
        let response = fixture.post(chat_request(streaming));
        if streaming {
            let (_, chunks) = sse_body(response);
            let images = chunks
                .iter()
                .filter_map(|chunk| chunk["choices"][0]["delta"]["images"].as_array())
                .flatten()
                .collect::<Vec<_>>();
            assert_eq!(images.len(), 2);
            assert_eq!(images[1]["index"], 0);
            assert_eq!(
                images[1]["image_url"]["url"],
                "data:image/webp;base64,ZmluYWw="
            );
        } else {
            let body = json_body(response);
            assert_eq!(
                body["choices"][0]["message"]["images"][0]["image_url"]["url"],
                "data:image/webp;base64,ZmluYWw="
            );
        }
    }
}

#[test]
fn output_logprobs_survive_json_projection_and_stream_snapshot_deduplication() {
    let probabilities =
        json!([{"token":"hello","logprob":-0.1,"bytes":[104,101,108,108,111],"top_logprobs":[]}]);
    let mut output = message("hello");
    output["content"][0]["logprobs"] = probabilities.clone();
    for streaming in [false, true] {
        let fixture = Fixture::new(Session::new(vec![
            created(),
            wire(
                "response.output_text.delta",
                json!({"output_index":0,"content_index":0,"item_id":"msg_chat_test","delta":"hello","logprobs":probabilities}),
            ),
            wire(
                "response.output_text.done",
                json!({"output_index":0,"content_index":0,"item_id":"msg_chat_test","text":"hello","logprobs":probabilities}),
            ),
            completed(json!([output])),
        ]));
        let response = fixture.post(chat_request(streaming));
        if streaming {
            let (_, chunks) = sse_body(response);
            let values = chunks
                .iter()
                .filter_map(|chunk| chunk["choices"][0]["logprobs"]["content"].as_array())
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(json!(values), probabilities);
        } else {
            assert_eq!(
                json_body(response)["choices"][0]["logprobs"]["content"],
                probabilities
            );
        }
    }
}

#[test]
fn streaming_citation_offsets_follow_delivered_text_when_messages_interleave() {
    let citation = json!({"type":"url_citation","start_index":2,"end_index":4,"url":"https://example.com","title":"source"});
    let mut first = message("abcd");
    first["content"][0]["annotations"] = json!([citation]);
    let mut second = message("xy");
    second["id"] = json!("msg_second");
    let fixture = Fixture::new(Session::new(vec![
        created(),
        wire(
            "response.output_text.delta",
            json!({"output_index":0,"content_index":0,"item_id":"msg_chat_test","delta":"ab"}),
        ),
        wire(
            "response.output_text.delta",
            json!({"output_index":1,"content_index":0,"item_id":"msg_second","delta":"xy"}),
        ),
        wire(
            "response.output_text.delta",
            json!({"output_index":0,"content_index":0,"item_id":"msg_chat_test","delta":"cd"}),
        ),
        wire(
            "response.output_text.annotation.added",
            json!({"output_index":0,"content_index":0,"item_id":"msg_chat_test","annotation_index":0,"annotation":citation}),
        ),
        completed(json!([first, second])),
    ]));
    let (_, chunks) = sse_body(fixture.post(chat_request(true)));
    assert_eq!(combined(&chunks, "content"), "abxycd");
    let annotations = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["annotations"].as_array())
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0]["url_citation"]["start_index"], 4);
    assert_eq!(annotations[0]["url_citation"]["end_index"], 6);
}
