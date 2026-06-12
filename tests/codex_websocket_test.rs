use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::Mutex};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request as WsRequest, Response as WsResponse},
        Message,
    },
};

use codex_proxy_rs::codex::{
    client::{build_reqwest_client, CodexBackendClient, CodexRequestContext},
    types::CodexResponsesRequest,
    websocket::{ensure_http_sse_supported, transport_for_request, CodexTransport},
};

#[test]
fn transport_for_request_should_allow_http_sse_without_websocket_only_fields() {
    let request = base_request();

    assert_eq!(transport_for_request(&request), CodexTransport::HttpSse);
    assert!(ensure_http_sse_supported(&request).is_ok());
}

#[test]
fn transport_for_request_should_require_websocket_for_previous_response_id() {
    let mut request = base_request();
    request.previous_response_id = Some("resp_123".to_string());

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketRequired
    );
    assert_eq!(
        ensure_http_sse_supported(&request).unwrap_err().to_string(),
        "previous_response_id requires Codex WebSocket transport"
    );
}

#[test]
fn use_websocket_should_not_serialize_to_upstream_json() {
    let mut request = base_request();
    request.use_websocket = true;

    let body = serde_json::to_value(&request).unwrap();

    assert!(body.get("use_websocket").is_none());
    assert!(body.get("useWebSocket").is_none());
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn previous_response_id_should_use_websocket_transport() {
    let received_headers = Arc::new(Mutex::new(None));
    let received_request = Arc::new(Mutex::new(None));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let headers_for_task = Arc::clone(&received_headers);
    let request_for_task = Arc::clone(&received_request);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |request: &WsRequest, response: WsResponse| {
                let mut headers = Vec::new();
                for (name, value) in request.headers() {
                    let value = value.to_str().unwrap_or_default().to_string();
                    headers.push((name.as_str().to_string(), value));
                }
                let headers_for_callback = Arc::clone(&headers_for_task);
                tokio::spawn(async move {
                    *headers_for_callback.lock().await = Some(headers);
                });
                Ok(response)
            })
            .await
            .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let text = message.into_text().unwrap();
        *request_for_task.lock().await = Some(serde_json::from_str::<Value>(&text).unwrap());
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_ws",
                        "object": "response",
                        "usage": {
                            "input_tokens": 2,
                            "output_tokens": 1
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", Vec::new());
    request.previous_response_id = Some("resp_prev".to_string());
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws",
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: None,
            },
        )
        .await
        .unwrap();

    server.await.unwrap();
    let request = received_request.lock().await.clone().unwrap();
    assert_eq!(request["type"], "response.create");
    assert_eq!(request["model"], "gpt-5.5");
    assert_eq!(request["instructions"], "be brief");
    assert_eq!(request["previous_response_id"], "resp_prev");
    assert_eq!(request["stream"], true);
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws\""));
    assert_eq!(response.usage.unwrap().input_tokens, 2);
    let headers = received_headers.lock().await.clone().unwrap();
    assert!(headers
        .iter()
        .any(|(name, value)| { name == "authorization" && value == "Bearer access-token" }));
}

fn base_request() -> CodexResponsesRequest {
    CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new())
}
