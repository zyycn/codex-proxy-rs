use super::*;
use gateway_core::engine::response_control::{ResponseControl, ResponseInterruptError};

async fn next_json(socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> Value {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            Message::Pong(_) => {}
            other => panic!("unexpected synthetic upstream frame: {other:?}"),
        }
    }
}

fn interrupt_context(control: ResponseControl, previous: Option<&str>) -> AttemptContext {
    let account = ProviderAccountId::new("acct_provider_contract").unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_interrupt").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_response_control(Some(control)),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(
            BTreeSet::new(),
            None,
            previous.map(|_| ProviderAccountStateOwner::new(provider.clone(), account.clone())),
        )
        .with_account_scope(contract_account_scope()),
        previous.map(|id| {
            ContinuationBinding::Pinned(NativeContinuationPin::new(
                PreviousResponseId::new(id),
                PreviousResponseId::new(id),
                ClientApiKeyId::new("key_openai_contract").unwrap(),
                provider,
                account,
            ))
        }),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn response_interrupt_uses_the_active_socket_and_releases_control_before_reuse() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_codex_test_websocket(socket).await;
        for id in ["resp_interrupt_a", "resp_interrupt_b"] {
            let create = next_json(&mut socket).await;
            assert_eq!(create["type"], "response.create");
            if id == "resp_interrupt_b" {
                assert_eq!(create["previous_response_id"], "resp_interrupt_a");
            }
            socket
                .send(Message::Text(
                    json!({
                        "type": "response.created",
                        "response": {"id":id, "model":"gpt-5.4", "status":"in_progress"}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket.send(Message::Text(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"}).to_string().into())).await.unwrap();
            let interrupt = next_json(&mut socket).await;
            assert_eq!(
                interrupt,
                json!({"type":"response.interrupt", "response_id":id,"mode":"discard_partial_items"})
            );
            socket
                .send(Message::Text(
                    json!({
                        "type":"response.incomplete",
                        "response":{"id":id,"model":"gpt-5.4","status":"incomplete","incomplete_details":{"reason":"interrupted"},"output":[]}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
    });
    let provider = provider_with_base_url(&store, base_url);
    let mut session_state = None;
    for id in ["resp_interrupt_a", "resp_interrupt_b"] {
        let control = ResponseControl::default();
        let previous = (id == "resp_interrupt_b").then_some("resp_interrupt_a");
        let mut request = generate_with_persisted_session_context(
            "acct_provider_contract",
            "interrupt-conversation",
            "interrupt-session",
            "interrupt-thread",
        );
        if let Some(state) = session_state.take() {
            request = request.with_provider_session_state(state);
        }
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", Operation::Generate(request)),
                interrupt_context(control.clone(), previous),
            )
            .await
            .unwrap();
        let mut interrupted = false;
        timeout(Duration::from_secs(5), async {
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                if let Some(state) = event.session_update() {
                    session_state = Some(state.clone());
                }
                if !interrupted && control.interrupt(id).is_ok() {
                    assert_eq!(
                        control.interrupt("resp_foreign"),
                        Err(ResponseInterruptError::ResponseMismatch)
                    );
                    control.interrupt(id).unwrap();
                    interrupted = true;
                }
            }
        })
        .await
        .expect("interrupt completes the active response");
        assert!(interrupted);
        assert!(session_state.is_some());
        assert_eq!(
            control.interrupt(id),
            Err(ResponseInterruptError::Unavailable)
        );
    }
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
