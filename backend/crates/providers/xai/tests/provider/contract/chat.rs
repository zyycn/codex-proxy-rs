//! Chat 到 Grok 实际 Responses 的端到端合同

use super::*;
use gateway_protocol::chat_completions::{CHAT_PROTOCOL, ChatAccumulator};

#[tokio::test]
async fn chat_uses_actual_responses_and_restores_custom_arguments_only_for_chat() {
    let arguments = json!({"input":"echo 中文"}).to_string();
    let item = json!({"id":"fc1","type":"function_call","call_id":"call_1","name":"execute","arguments":arguments});
    let response = json!({"id":"resp_chat","model":MODEL,"status":"completed","output":[item.clone()],"usage":{"input_tokens":4,"output_tokens":3,"total_tokens":7}});
    let wire = [
        json!({"type":"response.created","response":{"id":"resp_chat","model":MODEL}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc1","call_id":"call_1","name":"execute","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"fc1","delta":arguments}),
        json!({"type":"response.function_call_arguments.done","output_index":0,"item_id":"fc1","arguments":arguments}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":response}),
    ].into_iter().map(|event|format!("event: {}\ndata: {}\n\n",event["type"].as_str().unwrap(),event)).collect::<String>();
    let transport =
        StubInferenceTransport::sequence([InferenceMode::SuccessBody(wire.into_bytes())]);
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let body = object(json!({
        "model":"public-grok", "messages":[
            {"role":"assistant","tool_calls":[{"id":"prior","type":"custom","custom":{"name":"execute","input":"pwd"}}]},
            {"role":"tool","tool_call_id":"prior","content":"/tmp"},
            {"role":"user","content":"now echo"}
        ],
        "tools":[{"type":"custom","custom":{"name":"execute","description":"Run text"}}],
        "tool_choice":{"type":"custom","custom":{"name":"execute"}},
        "stream":true,"stream_options":{"include_usage":true}
    }));
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(CHAT_PROTOCOL, body).unwrap(),
    ));
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", operation),
            context(CancellationToken::new(), None),
        )
        .await
        .unwrap();
    let mut accumulator = ChatAccumulator::default();
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        let terminal = event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
        completed |= terminal;
        for translated in stream.translate_native_response(event, terminal).unwrap() {
            if let Some(wire) = translated.wire_event() {
                assert_eq!(wire.protocol(), CHAT_PROTOCOL);
                assert_eq!(wire.data()["object"], "chat.completion.chunk");
                accumulator.push(wire.data()).unwrap();
            }
        }
    }
    assert!(completed);
    let result = accumulator.finish().unwrap();
    assert_eq!(result["model"], "public-grok");
    assert_eq!(
        result["choices"][0]["message"]["tool_calls"][0],
        json!({"id":"call_1","type":"custom","custom":{"name":"execute","input":"echo 中文"}})
    );
    assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(result["usage"]["total_tokens"], 7);
    let requests = transport.requests.lock().unwrap();
    let sent: Value = serde_json::from_slice(requests[0].body()).unwrap();
    assert_eq!(sent["model"], MODEL);
    assert_eq!(sent["tools"][0]["type"], "function");
    assert_eq!(sent["tools"][0]["name"], "execute");
    assert_eq!(
        sent["tool_choice"],
        json!({"type":"function","name":"execute"})
    );
    assert_eq!(sent["input"][0]["type"], "function_call");
    assert_eq!(
        serde_json::from_str::<Value>(sent["input"][0]["arguments"].as_str().unwrap()).unwrap(),
        json!({"input":"pwd"})
    );
    assert_eq!(sent["input"][1]["type"], "function_call_output");
    assert_eq!(sent["tools"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn chat_allowed_custom_choice_matches_the_actual_function_declaration() {
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(ProtocolPayload::json_object(CHAT_PROTOCOL,object(json!({
        "model":MODEL,"messages":[{"role":"user","content":"hello"}],
        "tools":[{"type":"custom","custom":{"name":"run"}}],
        "tool_choice":{"type":"allowed_tools","allowed_tools":{"mode":"required","tools":[{"type":"custom","custom":{"name":"run"}}]}}
    }))).unwrap()));
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", operation),
            context(CancellationToken::new(), None),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let requests = transport.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].body()).unwrap();
    assert_eq!(
        body["tool_choice"],
        json!({"type":"allowed_tools","mode":"required","tools":[{"type":"function","name":"run"}]})
    );
    assert_eq!(body["tools"][0]["name"], "run");
}

#[tokio::test]
async fn unsupported_custom_formats_keep_safe_fields_without_sending() {
    for tool in [
        json!({"type":"custom","custom":{"name":"run","format":{"type":"grammar","grammar":{"definition":"PRIVATE_GRAMMAR"}}}}),
        json!({"type":"custom","name":"run","format":{"type":"grammar","grammar":{"definition":"PRIVATE_GRAMMAR"}}}),
    ] {
        let transport = StubInferenceTransport::success();
        let provider = provider(StubSelector::success(), transport.clone()).await;
        let field = if tool.get("custom").is_some() {
            "tools[0].custom.format"
        } else {
            "tools[0].format"
        };
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                CHAT_PROTOCOL,
                object(json!({
                    "model":MODEL,"messages":[{"role":"user","content":"hello"}],"tools":[tool]
                })),
            )
            .unwrap(),
        ));
        let error = provider
            .execute(
                provider_request_with_operation("xai", operation),
                context(CancellationToken::new(), None),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        let public = gateway_core::error::GatewayError::from_provider(&error);
        assert_eq!(public.client_error_code(), Some("unsupported_parameter"));
        assert_eq!(public.client_error_parameter(), Some(field));
        assert!(!public.client_message().contains("PRIVATE_GRAMMAR"));
        assert!(transport.requests.lock().unwrap().is_empty());
    }
}
