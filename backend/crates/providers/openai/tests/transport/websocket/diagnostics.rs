//! 验证 WebSocket 异常断开诊断保留事实并脱敏正文

use super::*;
use gateway_core::diagnostics::TraceContext;

#[tokio::test]
async fn diagnostics_preserve_abrupt_disconnect_facts_without_response_content() {
    for (reuse, send_event) in [(false, false), (false, true), (true, false), (true, true)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_codex_test_websocket(stream).await;
            websocket.next().await.unwrap().unwrap();
            if reuse {
                websocket
                    .send(Message::Text(
                        completed_websocket_response("resp_warmup", 1, 1).into(),
                    ))
                    .await
                    .unwrap();
                websocket.next().await.unwrap().unwrap();
            }
            if send_event {
                websocket
                    .send(Message::Text(
                        json!({"type": "codex.response.metadata", "private": "private-response-body"})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            // 直接丢弃传输连接，不执行 WebSocket 关闭握手
        });
        let trace = TraceContext::new("req_abrupt_disconnect");
        let attempt = trace.attempt(1);
        let mut request = codex_request("gpt-5.5", "private-request-body", Vec::new());
        request.set_previous_response_id(Some("resp_previous".to_owned()));
        request.previous_response_scope = Some(PreviousResponseScope::Persisted);
        request.force_http_sse = false;
        request.local_conversation_id = Some("diagnostics-conversation".to_owned());
        let backend = CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            format!("http://{addr}"),
            test_wire_profile(),
        )
        .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_mins(1))));
        if reuse {
            backend
                .create_response(
                    &request,
                    request_context("req_warmup", Some("chatgpt-account")),
                )
                .await
                .expect("warmup response must complete");
        }
        let result = backend
            .create_response_stream(
                &request,
                request_context("req_abrupt_disconnect", Some("chatgpt-account"))
                    .with_trace(&attempt),
            )
            .await;
        let mut failed = result.is_err();
        if let Ok(response) = result {
            let mut body = response.body;
            while let Some(chunk) = body.next().await {
                if chunk.is_err() {
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed, "an abrupt disconnect must fail the exchange");
        server.await.unwrap();
        let snapshot = trace.snapshot().unwrap();
        let events = snapshot["events"].as_array().unwrap();
        let connection = events
            .iter()
            .find(|event| event["stage"] == "upstream.connection")
            .unwrap();
        let failure = events
            .iter()
            .find(|event| event["stage"] == "upstream.read.failed")
            .unwrap();
        assert_eq!(
            failure["data"]["failureReason"],
            "reset_without_closing_handshake"
        );
        assert_eq!(
            failure["data"]["connectionId"],
            connection["data"]["connectionId"]
        );
        assert_eq!(failure["data"]["reused"], reuse);
        assert_eq!(
            failure["data"]["lastEventType"],
            if send_event {
                json!("codex.response.metadata")
            } else {
                Value::Null
            }
        );
        assert!(failure["data"]["connectionAgeMs"].is_u64());
        assert!(failure["data"]["connectionIdleMs"].is_u64());
        assert!(!snapshot.to_string().contains("private-response-body"));
        assert!(!snapshot.to_string().contains("private-request-body"));
    }
}
