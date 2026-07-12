use std::{net::SocketAddr, time::Duration};

use axum::Router;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{
        Error as WebSocketError, Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};

use crate::dispatch::service::accept_async as accept_upstream_websocket;

struct TestServer {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn responses_websocket_should_reject_missing_client_api_key_during_handshake() {
    let (app, _api_key, _dir) = super::test_app_with_client_api_key().await;
    let server = spawn_app(app).await;
    let request = format!("ws://{}/v1/responses", server.address)
        .into_client_request()
        .unwrap();

    let error = connect_async(request).await.unwrap_err();

    let WebSocketError::Http(response) = error else {
        panic!("expected HTTP handshake rejection");
    };
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn responses_websocket_should_return_official_error_frame_when_accounts_are_unavailable() {
    let (app, api_key, _dir) = super::test_app_with_client_api_key().await;
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    websocket
        .send(Message::Text(response_create_payload("no accounts").into()))
        .await
        .unwrap();
    let events = receive_response(&mut websocket).await;

    assert_eq!(events.last().unwrap()["type"], "error");
    assert_eq!(events.last().unwrap()["status"], 503);
    assert_eq!(
        events.last().unwrap()["error"]["code"],
        "no_available_accounts"
    );
}

#[tokio::test]
async fn responses_websocket_should_forward_multiple_response_create_requests_on_one_connection() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_base_url = format!("http://{}", upstream_listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = upstream_listener.accept().await.unwrap();
        let mut websocket = accept_upstream_websocket(stream).await.unwrap();
        let mut requests = Vec::new();
        for index in 1..=2 {
            let request = websocket.next().await.unwrap().unwrap();
            requests.push(serde_json::from_str::<Value>(&request.into_text().unwrap()).unwrap());
            send_upstream_response(&mut websocket, &format!("resp_{index}"), "ok").await;
        }
        requests
    });
    let (app, api_key, _dir) =
        crate::dispatch::service::test_app_with_account(upstream_base_url).await;
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    websocket
        .send(Message::Text(response_create_payload("first").into()))
        .await
        .unwrap();
    let first = receive_response(&mut websocket).await;
    websocket
        .send(Message::Text(response_create_payload("second").into()))
        .await
        .unwrap();
    let second = receive_response(&mut websocket).await;
    let upstream_requests = upstream.await.unwrap();

    assert_eq!(response_event_types(&first), response_event_types(&second));
    assert_eq!(first.first().unwrap()["type"], "response.metadata");
    assert_eq!(first.last().unwrap()["type"], "response.completed");
    assert_eq!(upstream_requests.len(), 2);
    assert_eq!(upstream_requests[0]["type"], "response.create");
    assert_eq!(upstream_requests[1]["type"], "response.create");
}

#[tokio::test]
async fn responses_websocket_should_allow_official_retry_after_midstream_upstream_disconnect() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_base_url = format!("http://{}", upstream_listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = upstream_listener.accept().await.unwrap();
        let mut first = accept_upstream_websocket(first_stream).await.unwrap();
        let _request = first.next().await.unwrap().unwrap();
        first
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "partial",
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        drop(first);

        let (second_stream, _) = upstream_listener.accept().await.unwrap();
        let mut second = accept_upstream_websocket(second_stream).await.unwrap();
        let _request = second.next().await.unwrap().unwrap();
        send_upstream_response(&mut second, "resp_retry", "recovered").await;
    });
    let (app, api_key, _dir) =
        crate::dispatch::service::test_app_with_account(upstream_base_url).await;
    let server = spawn_app(app).await;

    let mut first = connect_responses_websocket(server.address, &api_key).await;
    first
        .send(Message::Text(response_create_payload("retry me").into()))
        .await
        .unwrap();
    let failed = receive_response(&mut first).await;
    drop(first);

    let mut second = connect_responses_websocket(server.address, &api_key).await;
    second
        .send(Message::Text(response_create_payload("retry me").into()))
        .await
        .unwrap();
    let recovered = receive_response(&mut second).await;
    upstream.await.unwrap();

    assert!(response_event_types(&failed).contains(&"response.output_text.delta"));
    assert_eq!(failed.last().unwrap()["type"], "response.failed");
    assert_eq!(
        failed.last().unwrap()["response"]["error"]["code"],
        "stream_disconnected"
    );
    assert_eq!(recovered.last().unwrap()["type"], "response.completed");
}

async fn spawn_app(app: Router) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer { address, task }
}

async fn connect_responses_websocket(
    address: SocketAddr,
    api_key: &str,
) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {api_key}")).unwrap(),
    );
    connect_async(request).await.unwrap().0
}

fn response_create_payload(content: &str) -> String {
    json!({
        "type": "response.create",
        "model": "gpt-5.5",
        "instructions": "test",
        "input": [{"role": "user", "content": content}],
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "reasoning": {"effort": "medium"},
        "prompt_cache_key": "downstream-websocket-test",
        "store": false,
        "stream": true,
        "include": [],
    })
    .to_string()
}

async fn send_upstream_response(
    websocket: &mut WebSocketStream<tokio::net::TcpStream>,
    response_id: &str,
    text: &str,
) {
    for event in [
        json!({
            "type": "response.created",
            "response": {"id": response_id, "status": "in_progress"},
        }),
        json!({
            "type": "response.output_text.delta",
            "delta": text,
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "object": "response",
                "status": "completed",
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 1,
                    "total_tokens": 4,
                },
            },
        }),
    ] {
        websocket
            .send(Message::Text(event.to_string().into()))
            .await
            .unwrap();
    }
}

async fn receive_response<S>(websocket: &mut WebSocketStream<S>) -> Vec<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut events = Vec::new();
    loop {
        let message = timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("timed out waiting for Responses WebSocket event")
            .expect("Responses WebSocket closed before a terminal event")
            .expect("failed to receive Responses WebSocket event");
        let Message::Text(payload) = message else {
            continue;
        };
        let event = serde_json::from_str::<Value>(&payload).unwrap();
        let terminal = matches!(
            event.get("type").and_then(Value::as_str),
            Some("response.completed" | "response.incomplete" | "response.failed" | "error")
        );
        events.push(event);
        if terminal {
            return events;
        }
    }
}

fn response_event_types(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str))
        .collect()
}
