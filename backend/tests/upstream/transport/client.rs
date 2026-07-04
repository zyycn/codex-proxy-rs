use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::{accept_codex_test_websocket, completed_websocket_response, request_context};
use codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest;
use codex_proxy_rs::upstream::transport::{CodexBackendClient, CodexWebSocketPool};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn codex_backend_client_should_apply_configured_websocket_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_runtime_pool_first", 3, 1).into(),
            ))
            .await
            .unwrap();

        tokio::select! {
            second_message = first_websocket.next() => {
                let _second_message = second_message.unwrap().unwrap();
                first_websocket
                    .send(Message::Text(
                        completed_websocket_response("resp_runtime_pool_second", 3, 1).into(),
                    ))
                    .await
                    .unwrap();
                first_websocket.close(None).await.unwrap();
            }
            accepted = listener.accept() => {
                let (second_stream, _) = accepted.unwrap();
                accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
                let mut second_websocket = accept_codex_test_websocket(second_stream).await;
                let _second_message = second_websocket.next().await.unwrap().unwrap();
                second_websocket
                    .send(Message::Text(
                        completed_websocket_response("resp_runtime_pool_second", 3, 1).into(),
                    ))
                    .await
                    .unwrap();
                second_websocket.close(None).await.unwrap();
                first_websocket.close(None).await.unwrap();
            }
        }
    });
    let pool = CodexWebSocketPool::new(8, std::time::Duration::from_mins(1));
    let backend = CodexBackendClient::new(
        reqwest::Client::new(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(pool));
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", Vec::new());
    request.force_http_sse = false;
    request.set_previous_response_id(Some("resp_runtime_pool_previous".to_string()));
    request.set_prompt_cache_key(Some("conversation-runtime-pool".to_string()));

    let first = backend
        .create_response(
            &request,
            request_context("req_runtime_pool_first", Some("chatgpt-account")),
        )
        .await
        .expect("first runtime websocket response should succeed");
    let second = backend
        .create_response(
            &request,
            request_context("req_runtime_pool_second", Some("chatgpt-account")),
        )
        .await
        .expect("second runtime websocket response should reuse configured pool");
    server.await.unwrap();

    assert!(first.body.contains("resp_runtime_pool_first"));
    assert!(second.body.contains("resp_runtime_pool_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 1);
}
