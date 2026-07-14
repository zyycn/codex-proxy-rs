use std::{fs, net::SocketAddr, path::Path, time::Duration};

use axum::Router;
use codex_proxy_rs::fleet::account::AccountStatus;
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

use crate::dispatch::service::{
    accept_async as accept_upstream_websocket, accept_websocket_with_authorization,
};

struct TestServer {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[test]
fn responses_websocket_replay_policy_should_be_owned_by_history_controller() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let api = fs::read_to_string(manifest.join("src/api/client/responses/websocket.rs")).unwrap();
    let service = fs::read_to_string(manifest.join("src/dispatch/service.rs")).unwrap();
    let history = fs::read_to_string(manifest.join("src/dispatch/controllers/history.rs")).unwrap();
    let protocol =
        fs::read_to_string(manifest.join("src/upstream/openai/protocol/responses.rs")).unwrap();

    assert!(api.contains(".prepare_connection_replay("));
    assert!(api.contains(".commit_connection_replay("));
    for forbidden in [
        "ConnectionReplayState",
        "PendingReplayUpdate",
        "sanitize_replay_items",
        ".previous_response_id()",
        "dispatch::controllers",
    ] {
        assert!(
            !api.contains(forbidden),
            "Responses WebSocket API must not own replay policy {forbidden}"
        );
    }
    for owned in [
        "enum ConnectionReplayUpdate",
        "ConnectionReplayUpdate::Replace",
        "ConnectionReplayUpdate::Append",
        "ConnectionReplayUpdate::Unavailable",
        "sanitize_replay_items",
        "snapshot.last_response_id.as_deref() == Some(previous_response_id)",
    ] {
        assert!(
            history.contains(owned),
            "HistoryController owner must contain {owned}"
        );
    }
    for boundary in [
        "pub(crate) struct ConnectionReplaySnapshot",
        "pub(crate) struct ConnectionReplayPlan",
        "pub(crate) struct ConnectionTranscriptFacts",
        "fn prepare_connection_replay(",
        "fn commit_connection_replay(",
    ] {
        assert!(
            service.contains(boundary),
            "ResponseDispatchService must expose narrow replay boundary {boundary}"
        );
    }
    assert!(!protocol.contains("sanitize_replay_items"));
}

#[tokio::test]
async fn responses_websocket_should_reject_missing_client_api_key_during_handshake() {
    let (app, _api_key, _dir, _connections) = super::test_app_with_client_api_key().await;
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
    let (app, api_key, _dir, _connections) = super::test_app_with_client_api_key().await;
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
async fn responses_websocket_should_preserve_upstream_client_failure() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_base_url = format!("http://{}", upstream_listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = upstream_listener.accept().await.unwrap();
        let mut websocket = accept_upstream_websocket(stream).await.unwrap();
        let _request = receive_upstream_request(&mut websocket).await;
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_server_overloaded",
                        "status": "failed",
                        "error": {
                            "code": "server_overloaded",
                            "message": "upstream is temporarily overloaded",
                        },
                    },
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let (app, api_key, _dir) =
        crate::dispatch::service::test_app_with_account(upstream_base_url).await;
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    websocket
        .send(Message::Text(response_create_payload("overload").into()))
        .await
        .unwrap();
    let events = receive_response(&mut websocket).await;
    upstream.await.unwrap();
    let failure = events.last().unwrap();

    assert_eq!(failure["type"], "error");
    assert_eq!(failure["status"], 503);
    assert_eq!(failure["error"]["code"], "server_overloaded");
    assert_eq!(
        failure["error"]["message"],
        "upstream is temporarily overloaded"
    );
}

#[tokio::test]
async fn responses_websocket_should_disconnect_when_connection_draining_starts() {
    let (app, api_key, _dir, connection_drain) = super::test_app_with_client_api_key().await;
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    assert_eq!(connection_drain.begin_shutdown(), 1);
    tokio::time::timeout(Duration::from_secs(1), connection_drain.wait())
        .await
        .expect("Responses WebSocket task should stop during connection draining");
    let disconnected = tokio::time::timeout(Duration::from_secs(1), websocket.next())
        .await
        .expect("Responses WebSocket client should observe the disconnected connection");
    assert!(matches!(
        disconnected,
        None | Some(Err(_)) | Some(Ok(Message::Close(_)))
    ));
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
async fn responses_websocket_cyber_terminal_should_change_the_immediate_next_request() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_base_url = format!("http://{}", upstream_listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (primary_stream, _) = upstream_listener.accept().await.unwrap();
        let mut primary =
            accept_websocket_with_authorization(primary_stream, "Bearer access-primary").await;
        let primary_request = receive_upstream_request(&mut primary).await;
        for event in [
            json!({
                "type": "response.created",
                "response": {"id": "resp_downstream_ws_cyber", "status": "in_progress"},
            }),
            json!({
                "type": "response.output_text.delta",
                "delta": "partial downstream WebSocket output",
            }),
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_downstream_ws_cyber",
                    "status": "failed",
                    "error": {
                        "code": "cyber_policy",
                        "message": "This request has been flagged for possible cybersecurity risk.",
                    },
                },
            }),
        ] {
            primary
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        primary.close(None).await.unwrap();

        let (secondary_stream, _) = upstream_listener.accept().await.unwrap();
        let mut secondary =
            accept_websocket_with_authorization(secondary_stream, "Bearer access-secondary").await;
        let secondary_request = receive_upstream_request(&mut secondary).await;
        send_upstream_response(&mut secondary, "resp_downstream_ws_rotated", "rotated").await;
        (primary_request, secondary_request)
    });
    let (app, state, api_key, _pool, _dir) =
        crate::dispatch::service::test_app_with_two_accounts_and_state(upstream_base_url).await;
    assert!(
        state
            .services
            .account_pool
            .set_status("acct_secondary", AccountStatus::Disabled)
            .await
    );
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    websocket
        .send(Message::Text(response_create_payload("first").into()))
        .await
        .unwrap();
    let first = receive_response(&mut websocket).await;
    assert_eq!(first.last().unwrap()["type"], "response.failed");

    assert!(
        state
            .services
            .account_pool
            .set_status("acct_secondary", AccountStatus::Active)
            .await
    );
    websocket
        .send(Message::Text(response_create_payload("second").into()))
        .await
        .unwrap();
    let second = receive_response(&mut websocket).await;
    let (primary_request, secondary_request) = upstream.await.unwrap();

    assert_eq!(
        second.last().unwrap()["response"]["id"],
        "resp_downstream_ws_rotated"
    );
    assert_eq!(primary_request["input"][0]["content"], "first");
    assert_eq!(secondary_request["input"][0]["content"], "second");
}

#[tokio::test]
async fn responses_websocket_should_replay_connection_history_after_previous_response_not_found() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_base_url = format!("http://{}", upstream_listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = upstream_listener.accept().await.unwrap();
        let mut first = accept_upstream_websocket(first_stream).await.unwrap();
        let initial = receive_upstream_request(&mut first).await;
        send_upstream_response(&mut first, "resp_first", "first answer").await;

        let continued = receive_upstream_request(&mut first).await;
        first
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_missing",
                        "status": "failed",
                        "error": {
                            "code": "previous_response_not_found",
                            "message": "Previous response with id resp_first was not found",
                        },
                    },
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first.close(None).await.unwrap();

        let (replay_stream, _) = upstream_listener.accept().await.unwrap();
        let mut replay = accept_upstream_websocket(replay_stream).await.unwrap();
        let replayed = receive_upstream_request(&mut replay).await;
        send_upstream_response(&mut replay, "resp_recovered", "recovered").await;
        (initial, continued, replayed)
    });
    let (app, api_key, _dir) =
        crate::dispatch::service::test_app_with_account(upstream_base_url).await;
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    let mut first_payload: Value = serde_json::from_str(&response_create_payload("first")).unwrap();
    first_payload["input"][0]["id"] = json!("client_input_id");
    first_payload["input"][0]["encrypted_content"] = json!("client_secret");
    websocket
        .send(Message::Text(first_payload.to_string().into()))
        .await
        .unwrap();
    let first = receive_response(&mut websocket).await;
    assert_eq!(first.last().unwrap()["response"]["id"], "resp_first");

    let mut second_payload: Value =
        serde_json::from_str(&response_create_payload("second")).unwrap();
    second_payload["previous_response_id"] = json!("resp_first");
    websocket
        .send(Message::Text(second_payload.to_string().into()))
        .await
        .unwrap();
    let second = receive_response(&mut websocket).await;
    let (initial, continued, replayed) = upstream.await.unwrap();

    assert_eq!(second.last().unwrap()["response"]["id"], "resp_recovered");
    assert_eq!(initial["input"][0]["content"], "first");
    assert_eq!(continued["previous_response_id"], "resp_first");
    assert_eq!(continued["input"][0]["content"], "second");
    assert!(replayed.get("previous_response_id").is_none());
    assert_eq!(replayed["input"][0]["content"], "first");
    assert_eq!(replayed["input"][1]["role"], "assistant");
    assert_eq!(replayed["input"][2]["content"], "second");
    let replayed_json = replayed.to_string();
    assert!(!replayed_json.contains("client_input_id"));
    assert!(!replayed_json.contains("client_secret"));
    assert!(!replayed_json.contains("assistant_output_resp_first"));
}

#[tokio::test]
async fn responses_websocket_history_retry_should_not_fallback_when_owner_account_becomes_unavailable()
 {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_base_url = format!("http://{}", upstream_listener.local_addr().unwrap());
    let (continued_tx, continued_rx) = tokio::sync::oneshot::channel();
    let (release_failure_tx, release_failure_rx) = tokio::sync::oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (owner_stream, _) = upstream_listener.accept().await.unwrap();
        let mut owner = accept_upstream_websocket(owner_stream).await.unwrap();
        let initial = receive_upstream_request(&mut owner).await;
        send_upstream_response(&mut owner, "resp_owner", "first answer").await;

        let continued = receive_upstream_request(&mut owner).await;
        continued_tx.send(()).unwrap();
        release_failure_rx.await.unwrap();
        owner
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_owner_missing",
                        "status": "failed",
                        "error": {
                            "code": "previous_response_not_found",
                            "message": "Previous response with id resp_owner was not found",
                        },
                    },
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        owner.close(None).await.unwrap();

        let fallback = match timeout(Duration::from_millis(700), upstream_listener.accept()).await {
            Ok(Ok((fallback_stream, _))) => {
                let mut fallback = accept_upstream_websocket(fallback_stream).await.unwrap();
                let request = receive_upstream_request(&mut fallback).await;
                send_upstream_response(&mut fallback, "resp_wrong_fallback", "wrong account").await;
                Some(request)
            }
            Ok(Err(error)) => panic!("fallback listener failed: {error}"),
            Err(_) => None,
        };
        (initial, continued, fallback)
    });
    let (app, state, api_key, _pool, _dir) =
        crate::dispatch::service::test_app_with_two_accounts_and_state(upstream_base_url).await;
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    websocket
        .send(Message::Text(response_create_payload("first").into()))
        .await
        .unwrap();
    let first = receive_response(&mut websocket).await;
    let mut continued_payload: Value =
        serde_json::from_str(&response_create_payload("second")).unwrap();
    continued_payload["previous_response_id"] = json!("resp_owner");
    websocket
        .send(Message::Text(continued_payload.to_string().into()))
        .await
        .unwrap();
    continued_rx.await.unwrap();
    assert!(
        state
            .services
            .account_pool
            .set_status("acct_primary", AccountStatus::Expired)
            .await
    );
    release_failure_tx.send(()).unwrap();
    let second = receive_response(&mut websocket).await;
    let (initial, continued, fallback) = upstream.await.unwrap();

    assert_eq!(first.last().unwrap()["response"]["id"], "resp_owner");
    assert_eq!(initial["input"][0]["content"], "first");
    assert_eq!(continued["previous_response_id"], "resp_owner");
    assert_eq!(second.last().unwrap()["type"], "error");
    assert_eq!(
        second.last().unwrap()["error"]["code"],
        "previous_response_unavailable"
    );
    assert!(
        fallback.is_none(),
        "same-account retry must not fall back to another candidate: {fallback:?}"
    );
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
                "output": [{
                    "type": "message",
                    "id": format!("assistant_output_{response_id}"),
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                }],
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

async fn receive_upstream_request(websocket: &mut WebSocketStream<tokio::net::TcpStream>) -> Value {
    let request = websocket.next().await.unwrap().unwrap();
    serde_json::from_str(&request.into_text().unwrap()).unwrap()
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
