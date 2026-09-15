use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use futures::future::BoxFuture;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionService, StartExecution,
    StartProviderExecution, StartedExecution,
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::operation::Operation;
use gateway_core::routing::PublicModelId;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::openai::{api_router, authenticated_client};

#[derive(Default)]
struct Capture {
    request: Mutex<Option<(Value, Value, String)>>,
}

impl ExecutionService for Capture {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        if plaintext == "sk_chat_request_test" {
            Ok(authenticated_client(plaintext))
        } else {
            Err(ClientAuthenticationError::InvalidKey)
        }
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        vec![PublicModelId::new("model-a").expect("model")]
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, model: &PublicModelId) -> bool {
        model.as_str() == "model-a"
    }

    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let Operation::Generate(generation) = request.operation else {
                panic!("Chat must use the shared Generate operation")
            };
            let payload = generation.protocol_payload();
            *self.request.lock().expect("capture") = Some((
                Value::Object(payload.body().clone()),
                Value::Object(payload.context().clone()),
                request.metadata.endpoint,
            ));
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "request captured",
            ))
        })
    }

    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { panic!("Chat must not bypass shared Generate execution") })
    }
}

async fn send(body: Vec<u8>, headers: HeaderMap) -> (StatusCode, Value, Arc<Capture>) {
    let capture = Arc::new(Capture::default());
    let router = api_router(capture.clone()).await;
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer sk_chat_request_test")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request");
    request.headers_mut().extend(headers);
    let response = router.oneshot(request).await.expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&body).expect("JSON response"),
        capture,
    )
}

async fn converted(request: Value) -> Value {
    let (status, _, capture) = send(
        serde_json::to_vec(&request).expect("JSON"),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "capture terminates before execution"
    );
    let request = capture
        .request
        .lock()
        .expect("capture")
        .take()
        .expect("started execution");
    assert_eq!(request.2, "/v1/chat/completions");
    request.0
}

#[tokio::test]
async fn maps_common_controls_and_filters_protocol_extensions() {
    let schema =
        json!({"type":"object","properties":{"agent":{"type":"string"}},"custom":{"keep":true}});
    let body = converted(json!({"model":"model-a","messages":[{"role":"user","name":"caller","agent":"craft","content":[{"type":"text","text":"hi","agent":"local"}]}],
        "temperature":1.2,"top_p":0.8,"max_tokens":20,"max_completion_tokens":7,"reasoning_effort":"max","service_tier":"priority","instructions":"follow rules",
        "prompt_cache_key":"cache","prompt_cache_retention":"24h","user":"caller","safety_identifier":"safe","metadata":{"a":"b"},"unknown":{"secret":true},
        "tools":[{"type":"function","extension":true,"function":{"name":"run","parameters":schema,"strict":true,"agent":"local"}}],
        "tool_choice":{"type":"function","function":{"name":"run","agent":"local"},"extension":1},
        "stream":true,"stream_options":{"include_usage":true,"include_obfuscation":true,"extension":1},"stop":[],"top_logprobs":0
    })).await;
    assert_eq!(body["max_output_tokens"], 7);
    assert_eq!(body["temperature"], 1.2);
    assert_eq!(body["reasoning"], json!({"effort":"max"}));
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["instructions"], "follow rules");
    assert_eq!(body["prompt_cache_key"], "cache");
    assert_eq!(body["prompt_cache_retention"], "24h");
    assert_eq!(
        body["tools"][0],
        json!({"type":"function","name":"run","parameters":schema,"strict":true})
    );
    assert_eq!(
        body["input"],
        json!([{"role":"user","content":[{"type":"input_text","text":"hi"}]}])
    );
    for field in [
        "user",
        "metadata",
        "safety_identifier",
        "unknown",
        "stream_options",
        "stop",
        "top_logprobs",
    ] {
        assert!(body.get(field).is_none(), "{field}");
    }
}

#[tokio::test]
async fn preserves_file_inputs_refusals_and_public_reasoning_history() {
    let body = converted(json!({"model":"model-a","messages":[
        {"role":"user","content":[{"type":"file","file":{"file_data":"data:application/pdf;base64,AA==","filename":"a.pdf","agent":"local"}},{"type":"file","file":{"file_id":"file-1"}}]},
        {"role":"assistant","content":[{"type":"text","text":"first"},{"type":"refusal","refusal":"cannot"},{"type":"text","text":"last"}]},
        {"role":"assistant","refusal":"no"},
        {"role":"assistant","reasoning_content":"public summary","reasoning":"fallback","content":"answer"},
        {"role":"user","content":null}
    ]})).await;
    assert_eq!(
        body["input"][0]["content"],
        json!([{"type":"input_file","file_data":"data:application/pdf;base64,AA==","filename":"a.pdf"},{"type":"input_file","file_id":"file-1"}])
    );
    assert_eq!(
        body["input"][1],
        json!({"type":"message","id":"msg_chat_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"first","annotations":[]},{"type":"refusal","refusal":"cannot"},{"type":"output_text","text":"last","annotations":[]}]})
    );
    assert_eq!(
        body["input"][2]["content"],
        json!([{"type":"refusal","refusal":"no"}])
    );
    assert_eq!(
        body["input"][3],
        json!({"role":"assistant","content":[{"type":"input_text","text":"public summary"}]})
    );
    assert_eq!(body["input"][4]["content"], "answer");
    assert_eq!(body["input"][5]["content"], "");
}

#[tokio::test]
async fn pairs_repeated_legacy_calls_without_rewriting_business_data() {
    let body = converted(json!({"model":"model-a","functions":[{"name":"run","parameters":{}}],"function_call":{"name":"run"},"messages":[
        {"role":"assistant","content":"before","function_call":{"name":"run","arguments":""}},
        {"role":"function","name":"run","content":null},
        {"role":"assistant","function_call":{"name":"run","arguments":"{unfinished"}},
        {"role":"function","name":"run","content":"result"}
    ]})).await;
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["tool_choice"], json!({"type":"function","name":"run"}));
    assert_eq!(
        body["tools"],
        json!([{"type":"function","name":"run","parameters":{},"strict":false}])
    );
    let input = body["input"].as_array().expect("input");
    assert_eq!(input[0]["content"], "before");
    assert_eq!(input[1]["arguments"], "");
    assert_eq!(input[1]["call_id"], input[2]["call_id"]);
    assert_eq!(input[2]["output"], "");
    assert_eq!(input[3]["arguments"], "{unfinished");
    assert_eq!(input[3]["call_id"], input[4]["call_id"]);
    assert_ne!(input[1]["call_id"], input[3]["call_id"]);
}

#[tokio::test]
async fn preserves_responses_shaped_body_through_shared_execution() {
    let input = json!([{"type":"message","id":"msg_real","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"no"}]}]);
    let body=converted(json!({"model":"model-a","input":input,"metadata":{"original":"yes"},"user":"original","future":{"original":[1,2]},"store":false})).await;
    assert_eq!(body["input"], input);
    assert_eq!(body["stream"], false);
    assert_eq!(body["metadata"], json!({"original":"yes"}));
    assert_eq!(body["future"], json!({"original":[1,2]}));
}

#[tokio::test]
async fn rejects_invalid_compatibility_controls_and_ambiguous_histories() {
    for (extra, param) in [
        (json!({"temperature":2.1}), "temperature"),
        (json!({"max_tokens":0}), "max_tokens"),
        (
            json!({"max_completion_tokens":1.5}),
            "max_completion_tokens",
        ),
        (json!({"reasoning_effort":42}), "reasoning_effort"),
        (json!({"metadata":{"x":1}}), "metadata"),
        (json!({"input":[]}), "messages"),
        (
            json!({"functions":[{"name":"run"}],"parallel_tool_calls":true}),
            "parallel_tool_calls",
        ),
        (
            json!({"messages":[{"role":"function","name":"run","content":"unpaired"}]}),
            "messages[0].name",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"file","file":{"filename":"a.pdf"}}]}]}),
            "messages[0].content[0].file",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,  "}}]}]}),
            "messages[0].content[0].image_url.url",
        ),
    ] {
        let mut request = json!({"model":"model-a","messages":[{"role":"user","content":"hi"}]});
        request
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        let (status, body, capture) = send(
            serde_json::to_vec(&request).expect("JSON"),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["param"], param);
        assert!(capture.request.lock().expect("capture").is_none());
    }
}

#[tokio::test]
async fn preserves_message_order_images_and_opaque_function_arguments() {
    let body = converted(json!({
        "model":"model-a",
        "messages":[
            {"role":"system","content":"first"},
            {"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aGVsbG8=","detail":"high"}}]},
            {"role":"developer","content":"later instruction"},
            {"role":"assistant","content":"checking","tool_calls":[{"id":"call exact/1","type":"function","function":{"name":"lookup","arguments":"{incomplete"}}]},
            {"role":"tool","tool_call_id":"call exact/1","content":[{"type":"text","text":"part one"},{"type":"text","text":"part two"}]},
            {"role":"assistant","content":null,"tool_calls":[{"id":"call2","type":"function","function":{"name":"lookup","arguments":""}}]},
            {"role":"tool","tool_call_id":"call2","content":"done"}
        ]
    })).await;
    assert_eq!(body["stream"], false);
    assert_eq!(body["store"], false);
    assert!(body.get("messages").is_none());
    assert_eq!(
        body["input"],
        json!([
            {"role":"system","content":"first"},
            {"role":"user","content":[{"type":"input_text","text":"look"},{"type":"input_image","image_url":"data:image/png;base64,aGVsbG8=","detail":"high"}]},
            {"role":"developer","content":"later instruction"},
            {"role":"assistant","content":"checking"},
            {"type":"function_call","call_id":"call exact/1","name":"lookup","arguments":"{incomplete"},
            {"type":"function_call_output","call_id":"call exact/1","output":[{"type":"input_text","text":"part one"},{"type":"input_text","text":"part two"}]},
            {"type":"function_call","call_id":"call2","name":"lookup","arguments":""},
            {"type":"function_call_output","call_id":"call2","output":"done"}
        ])
    );
}

#[tokio::test]
async fn maps_function_tools_and_structured_output_without_rewriting_schema() {
    let schema = json!({"type":"object","properties":{"x":{"type":"string","future_schema_keyword":true}},"required":["x"],"additionalProperties":false});
    let body = converted(json!({
        "model":"model-a","messages":[{"role":"user","content":"hello"}],
        "tools":[{"type":"function","function":{"name":"lookup","description":"find","parameters":schema}}],
        "tool_choice":{"type":"function","function":{"name":"lookup"}},"parallel_tool_calls":false,
        "response_format":{"type":"json_schema","json_schema":{"name":"answer","description":"result","schema":schema,"strict":true}},
        "verbosity":"low","top_p":0.5
    })).await;
    assert_eq!(
        body["tools"],
        json!([{"type":"function","name":"lookup","description":"find","parameters":schema,"strict":false}])
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type":"function","name":"lookup"})
    );
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(
        body["text"],
        json!({"format":{"type":"json_schema","name":"answer","description":"result","schema":schema,"strict":true},"verbosity":"low"})
    );
    assert_eq!(body["top_p"], 0.5);
}

#[tokio::test]
async fn accepts_optional_nulls_and_neutral_defaults() {
    let body = converted(json!({
        "model":"model-a","messages":[{"role":"user","content":"hi","name":null}],
        "stream":null,"stream_options":null,"n":null,"store":false,"logprobs":false,
        "presence_penalty":0,"frequency_penalty":0.0,"logit_bias":{},"metadata":{},
        "service_tier":"auto","modalities":["text"],"temperature":null,"reasoning_effort":null,
        "max_tokens":null,"max_completion_tokens":null,"tools":null,"response_format":null
    }))
    .await;
    assert_eq!(
        body,
        json!({"model":"model-a","stream":false,"store":false,"input":[{"role":"user","content":"hi"}],"service_tier":"auto"})
    );
}

#[tokio::test]
async fn preserves_assistant_text_parts_and_clipped_tool_history() {
    let body = converted(json!({
        "model":"model-a", "n":1,
        "messages":[
            {"role":"assistant","content":[{"type":"text","text":"first"},{"type":"text","text":"second"}],"refusal":null},
            {"role":"tool","tool_call_id":"history-call","content":"result"},
            {"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.com/image.png"}}]}
        ], "response_format":{"type":"json_object"}
    })).await;
    assert_eq!(
        body["input"][0],
        json!({"role":"assistant","content":[{"type":"input_text","text":"first"},{"type":"input_text","text":"second"}]})
    );
    assert_eq!(body["input"][1]["call_id"], "history-call");
    assert_eq!(
        body["input"][2]["content"][0]["image_url"],
        "https://example.com/image.png"
    );
    assert_eq!(body["text"]["format"], json!({"type":"json_object"}));
}

#[tokio::test]
async fn rejects_unsupported_controls_without_starting_execution() {
    for (field, value) in [
        ("n", json!(2)),
        ("store", json!(true)),
        ("logprobs", json!(true)),
        ("stop", json!(["END"])),
    ] {
        let mut request = json!({"model":"model-a","messages":[{"role":"user","content":"hi"}]});
        request[field] = value;
        let (status, body, capture) = send(
            serde_json::to_vec(&request).expect("JSON"),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {body}");
        assert_eq!(body["error"]["param"], field);
        assert_eq!(body["error"]["code"], "unsupported_parameter");
        assert!(capture.request.lock().expect("capture").is_none());
        assert!(!body.to_string().contains("secret"));
    }
}

#[tokio::test]
async fn rejects_malformed_known_fields_and_unsupported_content() {
    for (extra, param) in [
        (
            json!({"messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"secret"}}]}]}),
            "messages[0].content[0].type",
        ),
        (
            json!({"tool_choice":{"type":"function","function":{"name":"undeclared"}}}),
            "tool_choice.function.name",
        ),
        (
            json!({"stream_options":{"include_usage":true}}),
            "stream_options",
        ),
        (
            json!({"stream":true,"stream_options":{"include_usage":"true"}}),
            "stream_options.include_usage",
        ),
        (json!({"tool_choice":"required","tools":[]}), "tool_choice"),
        (
            json!({"messages":[{"role":"tool","tool_call_id":"call"}]}),
            "messages[0].content",
        ),
        (
            json!({"response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{},"strict":"true"}}}),
            "response_format.json_schema.strict",
        ),
        (
            json!({"stream":true,"stream_options":{"include_obfuscation":"true"}}),
            "stream_options.include_obfuscation",
        ),
        (
            json!({"messages":[{"role":"assistant","content":12}]}),
            "messages[0].content",
        ),
        (
            json!({"messages":[{"role":"assistant","refusal":12}]}),
            "messages[0].refusal",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"refusal","refusal":12}]}]}),
            "messages[0].content[0].refusal",
        ),
    ] {
        let mut request = json!({"model":"model-a","messages":[{"role":"user","content":"hi"}]});
        request
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        let (status, body, capture) = send(
            serde_json::to_vec(&request).expect("JSON"),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["param"], param);
        assert!(capture.request.lock().expect("capture").is_none());
    }
}

#[tokio::test]
async fn ignores_message_extensions_without_filtering_tool_business_data() {
    let arguments = r#"{"agent":"craft","extensions":{"mode":"exact"}}"#;
    let schema = json!({"type":"object","properties":{"agent":{"type":"string"}},"extensions":{"agent":"schema-value"}});
    let request = json!({
        "model":"model-a",
        "messages":[
            {"role":"system","content":"system","client_id":"local"},
            {"role":"developer","content":"developer","annotations":[1,2]},
            {"role":"user","content":"question","agent":"craft","extensions":{"nested":true}},
            {"role":"assistant","content":null,"agent":"craft","tool_calls":[{"id":"call/unchanged","type":"function","function":{"name":"lookup","arguments":arguments}}]},
            {"role":"tool","tool_call_id":"call/unchanged","content":arguments,"local_state":false}
        ],
        "tools":[{"type":"function","function":{"name":"lookup","parameters":schema}}]
    });
    let mut baseline = request.clone();
    for message in baseline["messages"].as_array_mut().expect("messages") {
        message.as_object_mut().expect("message").retain(|key, _| {
            matches!(
                key.as_str(),
                "role" | "content" | "tool_calls" | "tool_call_id"
            )
        });
    }
    let body = converted(request).await;
    assert_eq!(body, converted(baseline).await);
    assert_eq!(
        body["input"][3],
        json!({"type":"function_call","call_id":"call/unchanged","name":"lookup","arguments":arguments})
    );
    assert_eq!(
        body["input"][4],
        json!({"type":"function_call_output","call_id":"call/unchanged","output":arguments})
    );
    assert_eq!(body["tools"][0]["parameters"], schema);
}

#[tokio::test]
async fn message_extensions_do_not_relax_known_fields_or_nested_validation() {
    for (message, param) in [
        (json!({"role":"invalid","content":"hi"}), "messages[0].role"),
        (json!({"content":"hi"}), "messages[0].role"),
        (json!({"role":"user"}), "messages[0].content"),
        (json!({"role":"user","content":42}), "messages[0].content"),
        (
            json!({"role":"assistant","tool_calls":{}}),
            "messages[0].tool_calls",
        ),
        (
            json!({"role":"tool","content":"result","tool_call_id":42}),
            "messages[0].tool_call_id",
        ),
        (
            json!({"role":"user","content":"hi","tool_calls":[]}),
            "messages[0].tool_calls",
        ),
        (
            json!({"role":"assistant","content":"hi","tool_call_id":"call"}),
            "messages[0].tool_call_id",
        ),
        (
            json!({"role":"tool","content":"result","tool_call_id":"call","tool_calls":[]}),
            "messages[0].tool_calls",
        ),
        (
            json!({"role":"user","content":"hi","name":12}),
            "messages[0].name",
        ),
        (
            json!({"role":"assistant","content":"hi","audio":{}}),
            "messages[0].audio",
        ),
        (
            json!({"role":"assistant","function_call":{}}),
            "messages[0].function_call.name",
        ),
        (
            json!({"role":"assistant","refusal":false}),
            "messages[0].refusal",
        ),
    ] {
        let mut message = message;
        message["agent"] = json!("craft");
        let request = json!({"model":"model-a","messages":[message]});
        let (status, body, capture) = send(
            serde_json::to_vec(&request).expect("JSON"),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["param"], param);
        assert!(capture.request.lock().expect("capture").is_none());
    }
}

#[tokio::test]
async fn ignores_null_tool_placeholders_without_relaxing_required_tool_id() {
    let body = converted(json!({"model":"model-a","messages":[
        {"role":"system","content":"system","tool_calls":null,"tool_call_id":null},
        {"role":"developer","content":"developer","tool_calls":null,"tool_call_id":null},
        {"role":"user","content":"question","tool_calls":null,"tool_call_id":null,"agent":"craft"},
        {"role":"assistant","content":"answer","tool_calls":null,"tool_call_id":null},
        {"role":"tool","tool_call_id":"call","content":"result","tool_calls":null}
    ]}))
    .await;
    assert_eq!(
        body["input"],
        json!([
            {"role":"system","content":"system"},
            {"role":"developer","content":"developer"},
            {"role":"user","content":"question"},
            {"role":"assistant","content":"answer"},
            {"type":"function_call_output","call_id":"call","output":"result"}
        ])
    );
    let request = json!({"model":"model-a","messages":[{"role":"tool","content":"result","tool_call_id":null,"agent":"craft"}]});
    let (status, body, capture) = send(
        serde_json::to_vec(&request).expect("JSON"),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["param"], "messages[0].tool_call_id");
    assert!(capture.request.lock().expect("capture").is_none());
}

#[tokio::test]
async fn decompresses_once_and_preserves_session_headers() {
    let request = br#"{"model":"model-a","messages":[{"role":"user","content":"hi"}]}"#;
    for encoding in ["gzip", "deflate", "zstd"] {
        let compressed = match encoding {
            "gzip" => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(request).expect("encode");
                encoder.finish().expect("finish")
            }
            "deflate" => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(request).expect("encode");
                encoder.finish().expect("finish")
            }
            _ => zstd::stream::encode_all(request.as_slice(), 1).expect("encode"),
        };
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", encoding.parse().expect("encoding"));
        headers.insert("session-id", "chat-session".parse().expect("session"));
        headers.insert("x-codex-turn-state", "chat-turn".parse().expect("turn"));
        let (status, _, capture) = send(compressed, headers).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let captured = capture
            .request
            .lock()
            .expect("capture")
            .take()
            .expect("decoded");
        assert_eq!(captured.0["input"][0]["content"], "hi");
        assert_eq!(captured.1["session_id"], "chat-session");
        assert_eq!(captured.1["turn_state"], "chat-turn");
    }
}
