use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use codex_proxy_adapters::codex::client;
use codex_proxy_core::{
    gateway::fingerprint::Fingerprint, protocol::codex::responses::CodexResponsesRequest,
};
use codex_proxy_platform::config::WebSocketPoolConfig;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        extensions::{compression::deflate::DeflateConfig, ExtensionsConfig},
        handshake::server::{Request as WsRequest, Response as WsResponse},
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
    let ws_pool = WebSocketPoolConfig {
        enabled: true,
        max_age_ms: 60_000,
        max_per_account: 8,
    };
    let backend = codex_proxy_runtime::upstream::codex_backend_client(
        format!("http://{addr}"),
        Fingerprint::default_for_tests(),
        &ws_pool,
    );
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", Vec::new());
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
    accept_hdr_async_with_config(
        stream,
        |_request: &WsRequest, mut response: WsResponse| {
            response.headers_mut().insert(
                "sec-websocket-extensions",
                "permessage-deflate".parse().unwrap(),
            );
            Ok(response)
        },
        Some(websocket_accept_config()),
    )
    .await
    .unwrap()
}

fn request_context<'a>(
    request_id: &'a str,
    account_id: Option<&'a str>,
) -> client::CodexRequestContext<'a> {
    client::CodexRequestContext {
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
            "usage": {
                "input_tokens": 3,
                "output_tokens": 1,
                "total_tokens": 4
            }
        }
    })
    .to_string()
}
