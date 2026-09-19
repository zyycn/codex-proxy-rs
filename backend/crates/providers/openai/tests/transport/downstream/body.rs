use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use provider_openai::encode_generate_request;

use super::super::*;

#[test]
fn encoder_should_adapt_pi_responses_parameters_without_losing_codex_fields() {
    // 对照本机 Pi 0.79.0 普通 Responses 适配在 onPayload 阶段生成的正文；
    // maxTokens、temperature 和长缓存选项最终会产生下面三个顶层字段。
    let body = json!({
        "model": "client-model",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hello"}]}],
        "stream": true,
        "store": false,
        "prompt_cache_key": "client-session",
        "prompt_cache_retention": "24h",
        "max_output_tokens": 512,
        "temperature": 0.2,
        "reasoning": {"effort": "high", "summary": "auto"},
        "include": ["reasoning.encrypted_content"]
    });
    let payload =
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload");
    let encoded = encode_generate_request(
        &GenerateRequest::from_protocol_payload(payload),
        "gpt-test",
        None,
    )
    .expect("encode Pi request");

    assert_eq!(
        Value::Object(encoded.body().clone()),
        json!({
            "model": "gpt-test",
            "input": body["input"],
            "stream": true,
            "store": false,
            "prompt_cache_key": "client-session",
            "reasoning": {"effort": "high", "summary": "auto"},
            "include": ["reasoning.encrypted_content"]
        })
    );
}

#[test]
fn encoder_should_expand_string_input_into_user_message_item() {
    // 公开 Responses API 的字符串 input 等价于一条 user 文本消息；
    // 目标形状对照 codex-rs/protocol/src/models.rs 的 ResponseItem::Message + ContentItem::InputText。
    let encoded = encode_downstream_request(json!({"model": "gpt-test", "input": "hello"}));

    assert_eq!(
        Value::Object(encoded.body().clone()),
        json!({
            "model": "gpt-test",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "store": false
        })
    );
    assert_eq!(encoded.input().len(), 1);
}

#[test]
fn encoder_should_keep_non_string_input_shapes_untouched() {
    // 数组按原样透传；空串仍展开为空文本消息，语义由上游判定。
    // null、对象等既不是官方形状也没有公开 API 的等价定义，不猜测语义。
    let items = json!([{"type": "message", "role": "user", "content": [
        {"type": "input_text", "text": "hi"},
        {"type": "input_image", "image_url": "data:image/png;base64,AA=="}
    ]}]);
    for input in [items, Value::Null, json!({"text": "hello"}), json!(42)] {
        let body = json!({"model": "gpt-test", "input": input, "store": false});
        let encoded = encode_downstream_request(body.clone());

        assert_eq!(Value::Object(encoded.body().clone()), body);
    }

    let encoded = encode_downstream_request(json!({"model": "gpt-test", "input": ""}));
    assert_eq!(
        encoded.body()["input"],
        json!([{"type": "message", "role": "user", "content": [{"type": "input_text", "text": ""}]}])
    );
}

#[test]
fn encoder_should_default_missing_store_without_changing_unknown_or_nested_fields() {
    let mut body = json!({
        "model": "client-model",
        "input": [{"role": "user", "content": [{
            "type": "input_text", "text": "temperature and max_output_tokens are tool parameter names"
        }]}],
        "max_tokens": 256,
        "future_options": {"temperature": 0.7},
        "client_metadata": {"prompt_cache_retention": "business-value", "store": true},
        "tools": [{
            "type": "function", "name": "configure_sampler",
            "parameters": {
                "type": "object",
                "properties": {
                    "temperature": {"type": "number"},
                    "max_output_tokens": {"type": "integer"},
                    "prompt_cache_retention": {"type": "string"}
                }
            }
        }]
    });
    let payload =
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload");
    let encoded = encode_generate_request(
        &GenerateRequest::from_protocol_payload(payload),
        "client-model",
        None,
    )
    .expect("encode business parameters");

    body["store"] = json!(false);
    assert_eq!(Value::Object(encoded.body().clone()), body);
}

#[test]
fn encoder_should_preserve_explicit_store_values() {
    for store in [json!(false), json!(true), Value::Null, json!("false")] {
        let body = json!({
            "model": "gpt-test",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hello"}]}],
            "store": store
        });
        let encoded = encode_downstream_request(body.clone());

        assert_eq!(Value::Object(encoded.body().clone()), body);
    }
}

#[test]
fn encoder_should_convert_system_messages_to_developer_without_changing_content() {
    let body = json!({
        "model": "client-model",
        "instructions": "Keep the existing instructions.",
        "input": [
            {
                "type": "message",
                "role": "system",
                "content": "You are a pirate.",
                "future_item_field": {"keep": true}
            },
            {
                "type": "message",
                "role": "system",
                "content": [{"type": "input_text", "text": "Be concise.", "role": "system"}]
            },
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Say hello."}]
            },
            {"type": "message", "role": "developer", "content": "Existing guidance."},
            {"role": "system", "content": "Shorthand message."},
            {"type": "computer_screenshot", "role": "system"},
            "opaque-item"
        ],
        "future_top_level_field": {"keep": true}
    });
    let encoded = encode_downstream_request(body);

    assert_eq!(
        Value::Object(encoded.body().clone()),
        json!({
            "model": "gpt-test",
            "instructions": "Keep the existing instructions.",
            "input": [
                {
                    "type": "message",
                    "role": "developer",
                    "content": "You are a pirate.",
                    "future_item_field": {"keep": true}
                },
                {
                    "type": "message",
                    "role": "developer",
                    "content": [{"type": "input_text", "text": "Be concise.", "role": "system"}]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "Say hello."}]
                },
                {"type": "message", "role": "developer", "content": "Existing guidance."},
                {"role": "system", "content": "Shorthand message."},
                {"type": "computer_screenshot", "role": "system"},
                "opaque-item"
            ],
            "store": false,
            "future_top_level_field": {"keep": true}
        })
    );
}

#[tokio::test]
async fn backend_http_should_send_default_store_and_normalized_system_role() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP server");
    let address = listener.local_addr().expect("HTTP server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept HTTP client");
        let raw = read_http_request_with_body(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        let head_end = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP head terminator")
            + 4;
        let body = zstd::stream::decode_all(&raw[head_end..]).expect("decode request body");
        serde_json::from_slice::<Value>(&body).expect("HTTP request JSON")
    });
    let mut request = encode_downstream_request(json!({
        "model": "gpt-test",
        "input": [{"type": "message", "role": "system", "content": "Be concise."}]
    }));
    request.force_http_sse = true;
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        test_wire_profile(),
    );

    client
        .create_response(
            &request,
            request_context("req_default_store_http", Some("test-account")),
        )
        .await
        .expect("HTTP response");

    assert_eq!(
        server.await.expect("HTTP server task"),
        json!({
            "model": "gpt-test",
            "input": [{"type": "message", "role": "developer", "content": "Be concise."}],
            "store": false,
            "stream": true
        })
    );
}

#[tokio::test]
async fn backend_websocket_should_send_default_store_and_normalized_system_role() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WebSocket server");
    let address = listener.local_addr().expect("WebSocket server address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept WebSocket client");
        let mut websocket = accept_codex_test_websocket(stream).await;
        let message = websocket
            .next()
            .await
            .expect("response.create")
            .expect("valid frame");
        let body = serde_json::from_str::<Value>(message.to_text().expect("text frame"))
            .expect("WebSocket request JSON");
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_default_store", 1, 1).into(),
            ))
            .await
            .expect("send terminal event");
        body
    });
    let mut request = encode_downstream_request(json!({
        "model": "gpt-test",
        "input": [{"type": "message", "role": "system", "content": "Be concise."}]
    }));
    request.use_websocket = true;
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_mins(1))));

    client
        .create_response(
            &request,
            request_context("req_default_store_ws", Some("test-account")),
        )
        .await
        .expect("WebSocket response");

    let body = server.await.expect("WebSocket server task");
    assert_eq!(body.get("store"), Some(&json!(false)));
    assert_eq!(
        body["input"],
        json!([{"type": "message", "role": "developer", "content": "Be concise."}])
    );
}

fn encode_downstream_request(body: Value) -> CodexResponsesRequest {
    let payload =
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload");
    encode_generate_request(
        &GenerateRequest::from_protocol_payload(payload),
        "gpt-test",
        None,
    )
    .expect("encode downstream request")
}
