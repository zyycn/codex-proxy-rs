use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use codex_proxy_rs::upstream::fingerprint::Fingerprint;
use codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest;
use codex_proxy_rs::upstream::transport::{
    CodexBackendClient, CodexRequestContext, CodexWebSocketPool,
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        extensions::{compression::deflate::DeflateConfig, ExtensionsConfig},
        handshake::server::{
            Callback, ErrorResponse, Request as WsRequest, Response as WsResponse,
        },
        protocol::WebSocketConfig,
        Message,
    },
};

#[tokio::test]
async fn codex_backend_client_should_apply_configured_websocket_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_runtime_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_runtime_pool_first").into(),
            ))
            .await
            .unwrap();

        tokio::select! {
            second_message = first_websocket.next() => {
                let _second_message = second_message.unwrap().unwrap();
                first_websocket
                    .send(Message::Text(
                        response_completed_websocket_message("resp_runtime_pool_second").into(),
                    ))
                    .await
                    .unwrap();
                first_websocket.close(None).await.unwrap();
            }
            accepted = listener.accept() => {
                let (second_stream, _) = accepted.unwrap();
                accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
                let mut second_websocket = accept_runtime_test_websocket(second_stream).await;
                let _second_message = second_websocket.next().await.unwrap().unwrap();
                second_websocket
                    .send(Message::Text(
                        response_completed_websocket_message("resp_runtime_pool_second").into(),
                    ))
                    .await
                    .unwrap();
                second_websocket.close(None).await.unwrap();
                first_websocket.close(None).await.unwrap();
            }
        }
    });
    let pool = CodexWebSocketPool::new(8, std::time::Duration::from_secs(60));
    let backend = CodexBackendClient::new(
        reqwest::Client::new(),
        format!("http://{addr}"),
        Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::new(pool));
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", Vec::new());
    request.force_http_sse = false;
    request.previous_response_id = Some("resp_runtime_pool_previous".to_string());
    request.prompt_cache_key = Some("conversation-runtime-pool".to_string());

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

fn websocket_accept_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

async fn accept_runtime_test_websocket(
    stream: TcpStream,
) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    accept_runtime_test_websocket_with(stream, |_request, response| {
        response.headers_mut().insert(
            "sec-websocket-extensions",
            "permessage-deflate".parse().unwrap(),
        );
    })
    .await
}

struct TestWebSocketCallback<F>(F);

impl<F> Callback for TestWebSocketCallback<F>
where
    F: FnOnce(&WsRequest, &mut WsResponse) + Unpin,
{
    fn on_request(
        self,
        request: &WsRequest,
        mut response: WsResponse,
    ) -> Result<WsResponse, ErrorResponse> {
        (self.0)(request, &mut response);
        Ok(response)
    }
}

async fn accept_runtime_test_websocket_with<F>(
    stream: TcpStream,
    callback: F,
) -> tokio_tungstenite::WebSocketStream<TcpStream>
where
    F: FnOnce(&WsRequest, &mut WsResponse) + Unpin,
{
    accept_hdr_async_with_config(
        stream,
        TestWebSocketCallback(callback),
        Some(websocket_accept_config()),
    )
    .await
    .unwrap()
}

fn request_context<'a>(
    request_id: &'a str,
    account_id: Option<&'a str>,
) -> CodexRequestContext<'a> {
    CodexRequestContext {
        access_token: "access-token",
        account_id,
        request_id,
        turn_state: None,
        turn_metadata: None,
        beta_features: None,
        include_timing_metrics: None,
        version: None,
        codex_window_id: None,
        parent_thread_id: None,
        cookie_header: None,
        installation_id: None,
        session_id: None,
    }
}

fn response_completed_websocket_message(response_id: &str) -> String {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "test response"
                }]
            }],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 1,
                "total_tokens": 4
            }
        }
    })
    .to_string()
}
