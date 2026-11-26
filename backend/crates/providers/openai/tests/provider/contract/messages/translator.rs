//! 转换错误保留安全字段与受限诊断，响应交付保持所选协议

use super::*;
use gateway_core::event::{ProtocolWireEvent, ProviderEvent};
use gateway_protocol::chat_completions::ChatError;
use std::error::Error as _;

#[tokio::test]
async fn chat_response_translation_preserves_causes_for_each_target() {
    for (target, protocol, operation, message) in [
        (
            ApiKeyProtocol::Messages,
            "anthropic",
            chat_operation(true),
            "The upstream response cannot be represented as a Chat completion.",
        ),
        (
            ApiKeyProtocol::Responses,
            "openai",
            chat_operation(true),
            "The upstream response cannot be represented as a Chat completion.",
        ),
    ] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        seed(&store, &server, target).await;
        let mut stream = provider(&store)
            .execute(
                planned_request("openai", operation),
                context("req_translation_cause", CancellationToken::new()),
            )
            .await
            .unwrap();
        let wire = ProtocolWireEvent::raw_json(
            protocol,
            Bytes::from_static(br#"{"private":"PRIVATE_INPUT"}"#),
        )
        .unwrap();
        let error = stream
            .translate_native_response(ProviderEvent::wire(wire), false)
            .unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::Protocol);
        assert_eq!(error.send_state(), UpstreamSendState::Sent);
        assert_eq!(
            error.source().unwrap().downcast_ref::<ChatError>(),
            Some(&ChatError::InvalidResponse)
        );
        assert!(error.error_details().unwrap().contains(message));
        assert!(!format!("{error:?} {error}").contains(message));
        assert!(!error.error_details().unwrap().contains("PRIVATE_INPUT"));
    }
}

#[tokio::test]
async fn request_translation_preserves_the_unsent_validation_cause() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, &server, ApiKeyProtocol::Messages).await;
    let body = json!({"model":"gpt-5.4","messages":[{"role":"user","content":"hello"}],"tool_choice":{"type":"allowed_tools","allowed_tools":{"mode":"required","tools":[]}}});
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("chat_completions", body.as_object().unwrap().clone())
            .unwrap(),
    ));
    let error = provider(&store)
        .execute(
            planned_request("openai", operation),
            context("req_translation_validation", CancellationToken::new()),
        )
        .await
        .err()
        .expect("invalid allowed tools");
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(matches!(
        error.source().unwrap().downcast_ref::<ChatError>(),
        Some(ChatError::InvalidValue { .. })
    ));
    let public = gateway_core::error::GatewayError::from_provider(&error);
    assert_eq!(public.client_error_code(), Some("invalid_value"));
    assert_eq!(
        public.client_error_parameter(),
        Some("tool_choice.allowed_tools.tools")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn complete_native_json_uses_the_requested_chat_delivery_and_usage_mode() {
    for target in [ApiKeyProtocol::Responses, ApiKeyProtocol::Messages] {
        for (streaming, include_usage) in [(false, false), (true, false), (true, true)] {
            let server = MockServer::start().await;
            let store = Arc::new(MemoryAccountStore::default());
            seed(&store, &server, target).await;
            let mut body = json!({"model":"gpt-5.4","stream":streaming,"messages":[{"role":"user","content":"hello"}]});
            if streaming {
                body["stream_options"] = json!({"include_usage":include_usage});
            }
            let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object("chat_completions", body.as_object().unwrap().clone())
                    .unwrap(),
            ));
            let mut stream = provider(&store)
                .execute(
                    planned_request("openai", operation),
                    context("req_chat_json", CancellationToken::new()),
                )
                .await
                .unwrap();
            let (source, body) = match target {
                ApiKeyProtocol::Responses => (
                    "openai",
                    json!({"id":"r","object":"response","model":"gpt-5.4","status":"completed","output":[{"id":"m","type":"message","role":"assistant","content":[{"type":"output_text","text":"hello","annotations":[]}]}],"usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}),
                ),
                ApiKeyProtocol::Messages => (
                    "anthropic",
                    json!({"id":"m","type":"message","role":"assistant","model":"gpt-5.4","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":2}}),
                ),
            };
            let wire =
                ProtocolWireEvent::raw_json(source, serde_json::to_vec(&body).unwrap().into())
                    .unwrap();
            let events = stream
                .translate_native_response(ProviderEvent::wire(wire), true)
                .unwrap();
            let mut accumulator = gateway_protocol::chat_completions::ChatAccumulator::default();
            let mut response = None;
            for wire in events.iter().filter_map(|event| event.wire_event()) {
                assert_eq!(wire.protocol(), "chat_completions");
                if streaming {
                    assert!(wire.raw_json_body().is_none());
                    assert_eq!(wire.data()["object"], "chat.completion.chunk");
                    accumulator.push(wire.data()).unwrap();
                } else {
                    response = Some(
                        serde_json::from_slice::<Value>(wire.raw_json_body().unwrap()).unwrap(),
                    );
                }
            }
            let response = response.unwrap_or_else(|| accumulator.finish().unwrap());
            assert_eq!(response["choices"][0]["message"]["content"], "hello");
            assert_eq!(response["choices"][0]["finish_reason"], "stop");
            if !streaming || include_usage {
                assert_eq!(response["usage"]["total_tokens"], 7);
            } else {
                assert!(response.get("usage").is_none());
            }
            drop(stream);
            assert!(server.received_requests().await.unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn non_streaming_chat_over_responses_sse_preserves_usage_without_stream_options() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, &server, ApiKeyProtocol::Responses).await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut stream = provider(&store)
        .execute(
            planned_request("openai", chat_operation(false)),
            context("req_chat_json_sse", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut accumulator = gateway_protocol::chat_completions::ChatAccumulator::default();
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        let terminal = event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
        for event in stream.translate_native_response(event, terminal).unwrap() {
            if let Some(wire) = event.wire_event() {
                accumulator.push(wire.data()).unwrap();
            }
        }
    }
    let result = accumulator.finish().unwrap();
    assert_eq!(result["usage"]["total_tokens"], 2);
    assert_eq!(result["choices"][0]["finish_reason"], "stop");
    let requests = server.received_requests().await.unwrap();
    let body = captured_request_body(&requests[0]);
    assert!(body.get("messages").is_none());
    assert_eq!(body["input"][0]["content"][0]["text"], "hello");
    server.verify().await;
}

#[tokio::test]
async fn explicitly_rewritten_native_wire_keeps_the_plugin_owned_response_protocol() {
    for target in [ApiKeyProtocol::Responses, ApiKeyProtocol::Messages] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        seed(&store, &server, target).await;
        let (protocol, endpoint, body, response) = match target {
            ApiKeyProtocol::Responses => (
                "openai",
                "/responses",
                json!({"model":"gpt-5.4","input":"plugin converted","stream":true}),
                CAPTURE_COMPLETED_SSE,
            ),
            ApiKeyProtocol::Messages => (
                "anthropic",
                "/messages",
                json!({"model":"gpt-5.4","messages":[{"role":"user","content":"plugin converted"}],"max_tokens":128,"stream":true}),
                MESSAGES_SSE,
            ),
        };
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_raw(response, "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;
        // 显式中间件已经完成唯一一次转换，Provider 只消费当前原生 wire
        let source = Operation::Generate(GenerateRequest::from_protocol_payload(ProtocolPayload::json_object("chat_completions", json!({"model":"gpt-5.4","messages":[{"role":"user","content":"hello"}],"stream":true}).as_object().unwrap().clone()).unwrap()));
        let operation = source
            .replace_middleware_wire(protocol, serde_json::to_vec(&body).unwrap().into())
            .unwrap();
        let Operation::Generate(generate) = &operation else {
            unreachable!()
        };
        assert_eq!(generate.source_protocol(), "chat_completions");
        let mut stream = provider(&store)
            .execute(
                planned_request("openai", operation),
                context("req_plugin_native", CancellationToken::new()),
            )
            .await
            .unwrap();
        let mut delivered = false;
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let terminal = event
                .canonical_facts()
                .iter()
                .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
            for event in stream.translate_native_response(event, terminal).unwrap() {
                if let Some(wire) = event.wire_event() {
                    assert_eq!(wire.protocol(), protocol);
                    delivered = true;
                }
            }
        }
        assert!(delivered);
        let sent = server.received_requests().await.unwrap();
        let sent = captured_request_body(&sent[0]);
        if target == ApiKeyProtocol::Responses {
            assert_eq!(sent["input"][0]["content"][0]["text"], "plugin converted");
        } else {
            assert_eq!(sent["messages"][0]["content"], "plugin converted");
        }
        server.verify().await;
    }
}

#[tokio::test]
async fn attempt_middleware_can_convert_chat_to_the_selected_native_responses_account() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, &server, ApiKeyProtocol::Responses).await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let rewrites = Arc::new(Mutex::new(0));
    let source = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "chat_completions",
            json!({"model":"gpt-5.4","messages":[{"role":"user","content":"hello"}],"stream":true})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    ));
    let mut stream = provider(&store)
        .execute(
            planned_request("openai", source),
            context_with_middleware(
                "req_plugin_attempt_native",
                Arc::new(RequestRewriteMiddleware {
                    rewrites: rewrites.clone(),
                    target: json!({"model":"gpt-5.4","input":"attempt converted","stream":true})
                        .as_object()
                        .unwrap()
                        .clone(),
                }),
                FastMode::Default,
            ),
        )
        .await
        .unwrap();
    let mut native_sse = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        if let Some(wire) = event.wire_event() {
            native_sse.extend_from_slice(wire.raw_sse_frame().unwrap());
        }
    }
    assert_eq!(native_sse, CAPTURE_COMPLETED_SSE.as_bytes());
    assert_eq!(*rewrites.lock().unwrap(), 1);
    let sent = server.received_requests().await.unwrap();
    assert_eq!(
        captured_request_body(&sent[0])["input"][0]["content"][0]["text"],
        "attempt converted"
    );
    server.verify().await;
}

#[tokio::test]
async fn rewritten_chat_cannot_authorize_a_builtin_converter_for_a_native_source() {
    for target in [ApiKeyProtocol::Responses, ApiKeyProtocol::Messages] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        seed(&store, &server, target).await;
        let operation = generate_operation()
            .replace_middleware_wire(
                "chat_completions",
                Bytes::from_static(
                    br#"{"model":"gpt-5.4","messages":[{"role":"user","content":"hello"}]}"#,
                ),
            )
            .unwrap();
        let error = provider(&store)
            .execute(
                planned_request("openai", operation),
                context("req_no_borrowed_chat", CancellationToken::new()),
            )
            .await
            .err()
            .expect("a native source cannot borrow the built-in Chat converter");
        assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn explicit_native_rewrite_still_requires_the_matching_account_protocol() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, &server, ApiKeyProtocol::Messages).await;
    let operation = chat_operation(true)
        .replace_middleware_wire(
            "openai",
            Bytes::from_static(br#"{"model":"gpt-5.4","input":"hello"}"#),
        )
        .unwrap();
    let error = provider(&store)
        .execute(
            planned_request("openai", operation),
            context("req_no_wrong_native_account", CancellationToken::new()),
        )
        .await
        .err()
        .expect("a native rewrite does not change account protocol eligibility");
    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(server.received_requests().await.unwrap().is_empty());
}
