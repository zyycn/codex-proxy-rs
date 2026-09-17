use super::*;

#[tokio::test]
async fn reused_websocket_should_keep_response_metadata_scoped_to_each_exchange() {
    const ROUNDS: usize = 40;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket_with(stream, |_, response| {
            for value in ["first", "second"] {
                response
                    .headers_mut()
                    .append("x-opening-multi", value.parse().unwrap());
            }
        })
        .await;
        for round in 0..ROUNDS {
            websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "type": "codex.response.metadata",
                        "headers": {
                            "x-models-etag": format!("etag-{round}"),
                            "openai-model": format!("model-{round}"),
                            "x-reasoning-included": "true",
                            "x-codex-safety-buffering-enabled": "true",
                            "x-codex-safety-buffering-faster-model": "faster-model",
                            "x-future-response-metadata": format!("future-{round}"),
                            "x-codex-turn-state": format!("turn-{round}")
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response(&format!("resp_metadata_{round}"), 1, 1).into(),
                ))
                .await
                .unwrap();
        }
    });
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(pool);
    let request = pooled_websocket_request("conversation-response-metadata");
    let expected = CodexResponseMetadata {
        client_headers: vec![
            (
                "x-opening-multi".to_owned(),
                bytes::Bytes::from_static(b"first"),
            ),
            (
                "x-opening-multi".to_owned(),
                bytes::Bytes::from_static(b"second"),
            ),
        ],
        ..CodexResponseMetadata::default()
    };

    for round in 0..ROUNDS {
        let response = timeout(
            Duration::from_secs(10),
            backend.create_response(
                &request,
                request_context("req_response_metadata", Some("chatgpt-account")),
            ),
        )
        .await
        .expect("response within timeout")
        .expect("response should complete on the same socket");
        assert_eq!(response.response_metadata, expected, "exchange {round}");
        assert_eq!(response.reported_model, Some(format!("model-{round}")));
        assert!(response.body.contains(&format!("etag-{round}")));
        assert_eq!(response.turn_state, Some(format!("turn-{round}")));
        if round > 0 {
            assert!(
                response
                    .websocket_pool_decision
                    .is_some_and(WebSocketPoolDecision::is_reuse)
            );
        }
    }
    server.await.unwrap();
}
