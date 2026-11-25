//! 搜索能力变化时的连接复用与原生续接恢复

mod contract;
use super::*;

async fn search_test_server() -> (
    CodexBackendClient,
    Arc<CodexWebSocketPool>,
    tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let mut connection = 0;
        while let Ok((socket, _)) = listener.accept().await {
            connection += 1;
            let mut websocket = accept_codex_test_websocket(socket).await;
            while let Some(Ok(message)) = websocket.next().await {
                match message {
                    Message::Text(text) => {
                        let request: Value = serde_json::from_str(&text).unwrap();
                        tx.send((connection, request)).unwrap();
                        websocket
                            .send(Message::Text(
                                completed_websocket_response("resp_search", 1, 1).into(),
                            ))
                            .await
                            .unwrap();
                    }
                    Message::Ping(_) => websocket.flush().await.unwrap(),
                    Message::Close(_) => {
                        websocket.flush().await.unwrap();
                        break;
                    }
                    other => panic!("unexpected frame: {other:?}"),
                }
            }
        }
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let client = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    (client, pool, rx, server)
}

fn search_request(downstream_ws: bool, overrides: Value) -> CodexResponsesRequest {
    let mut body = codex_request_body("gpt-test", "be brief", vec![]);
    body.insert("store".into(), json!(false));
    body.extend(overrides.as_object().unwrap().clone());
    let mut request = CodexResponsesRequest::from_body(body);
    request.local_conversation_id = Some("search-session".into());
    request.downstream_websocket_connection_id = downstream_ws.then(|| "search-lane".into());
    request
}

async fn send(
    client: &CodexBackendClient,
    request: &CodexResponsesRequest,
) -> CodexClientResult<CollectedBackendResponse> {
    timeout(
        Duration::from_secs(3),
        client.create_response(
            request,
            request_context("req_search", Some("search-account")),
        ),
    )
    .await
    .expect("request must finish without waiting for pool expiry")
}
