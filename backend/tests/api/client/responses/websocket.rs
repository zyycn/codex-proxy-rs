use std::{fs, net::SocketAddr, path::Path, time::Duration};

use axum::Router;
use chrono::Utc;
use codex_proxy_rs::fleet::{account::AccountStatus, cookies::PgCookieStore};
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
    accept_websocket_with_request_headers,
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
        "sanitize_cross_account_output",
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
        "struct CrossAccountReplay",
        "LocalReplayItem::ClientInput",
        "sanitize_cross_account_output",
        "project_transcript_to_account",
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
    assert!(protocol.contains("enum LocalReplayItem"));
    assert!(!protocol.contains("sanitize_cross_account_output"));
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
    first_payload["input"][0]["tool_arguments"] = json!({
        "id": "user_tool_argument_id",
        "encrypted_content": "user_tool_argument_secret"
    });
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
    assert_eq!(replayed["input"][0]["id"], "client_input_id");
    assert_eq!(replayed["input"][0]["encrypted_content"], "client_secret");
    assert_eq!(
        replayed["input"][0]["tool_arguments"]["id"],
        "user_tool_argument_id"
    );
    assert_eq!(
        replayed["input"][0]["tool_arguments"]["encrypted_content"],
        "user_tool_argument_secret"
    );
    assert!(replayed["input"][1].get("id").is_none());
}

#[tokio::test]
async fn responses_websocket_cross_account_replay_should_isolate_account_bound_state() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_base_url = format!("http://{}", upstream_listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (primary_stream, _) = upstream_listener.accept().await.unwrap();
        let (mut primary, primary_headers) =
            accept_websocket_with_request_headers(primary_stream, "Bearer access-primary").await;
        let initial_primary = receive_upstream_request(&mut primary).await;
        send_upstream_response_with_output(
            &mut primary,
            "resp_account_a",
            "answer from A",
            vec![
                json!({
                    "type": "message",
                    "id": "msg_account_a",
                    "role": "assistant",
                    "encrypted_content": "encrypted_message_a",
                    "content": [{
                        "type": "output_text",
                        "id": "nested_content_id_a",
                        "text": "answer from A"
                    }],
                }),
                json!({
                    "type": "reasoning",
                    "id": "reasoning_account_a",
                    "encrypted_content": "encrypted_reasoning_a",
                    "summary": [{
                        "type": "summary_text",
                        "id": "nested_summary_id_a",
                        "text": "reasoning summary A"
                    }],
                }),
                json!({
                    "type": "function_call",
                    "id": "function_account_a",
                    "call_id": "call_account_a",
                    "caller": "client",
                    "name": "lookup",
                    "arguments": "{\"id\":\"tool_argument_id_a\"}",
                }),
                json!({
                    "type": "compaction",
                    "id": "compaction_account_a",
                    "encrypted_content": "encrypted_compaction_a",
                }),
            ],
        )
        .await;

        let (secondary_stream, _) = upstream_listener.accept().await.unwrap();
        let (mut secondary, secondary_headers) =
            accept_websocket_with_request_headers(secondary_stream, "Bearer access-secondary")
                .await;
        let cross_account_replay = receive_upstream_request(&mut secondary).await;
        send_upstream_response_with_output(
            &mut secondary,
            "resp_account_b",
            "answer from B",
            vec![
                json!({
                    "type": "message",
                    "id": "msg_account_b",
                    "role": "assistant",
                    "encrypted_content": "encrypted_message_b",
                    "content": [{"type": "output_text", "text": "answer from B"}],
                }),
                json!({
                    "type": "reasoning",
                    "id": "reasoning_account_b",
                    "encrypted_content": "encrypted_reasoning_b",
                    "summary": [{"type": "summary_text", "text": "reasoning summary B"}],
                }),
            ],
        )
        .await;

        let continued_secondary = receive_upstream_request(&mut secondary).await;
        secondary
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_account_b_missing",
                        "status": "failed",
                        "error": {
                            "code": "previous_response_not_found",
                            "message": "Previous response with id resp_account_b was not found",
                        },
                    },
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        secondary.close(None).await.unwrap();

        let (retry_stream, _) = upstream_listener.accept().await.unwrap();
        let (mut retry, retry_headers) =
            accept_websocket_with_request_headers(retry_stream, "Bearer access-secondary").await;
        let same_account_replay = receive_upstream_request(&mut retry).await;
        send_upstream_response(&mut retry, "resp_account_b_recovered", "recovered on B").await;

        (
            primary_headers,
            secondary_headers,
            retry_headers,
            initial_primary,
            cross_account_replay,
            continued_secondary,
            same_account_replay,
        )
    });
    let (app, state, api_key, pool, _dir) =
        crate::dispatch::service::test_app_with_two_accounts_and_state(upstream_base_url).await;
    let cookie_store = PgCookieStore::new(pool);
    cookie_store
        .capture_set_cookie(
            "acct_primary",
            "cf_clearance=primary; Domain=.chatgpt.com; Path=/codex",
        )
        .await
        .unwrap();
    cookie_store
        .capture_set_cookie(
            "acct_secondary",
            "cf_clearance=secondary; Domain=.chatgpt.com; Path=/codex",
        )
        .await
        .unwrap();
    let server = spawn_app(app).await;
    let mut websocket = connect_responses_websocket(server.address, &api_key).await;

    let mut first_payload: Value = serde_json::from_str(&response_create_payload("first")).unwrap();
    first_payload["input"][0]["id"] = json!("client_input_id");
    first_payload["input"][0]["encrypted_content"] = json!("client_semantic_secret");
    first_payload["input"][0]["tool_arguments"] = json!({"id": "client_tool_argument_id"});
    first_payload["input"].as_array_mut().unwrap().push(json!({
        "type": "reasoning",
        "id": "client_replayed_reasoning_id_a",
        "encrypted_content": "client_replayed_reasoning_encrypted_a",
        "summary": [{
            "type": "summary_text",
            "id": "client_replayed_summary_id_a",
            "text": "client replayed summary A"
        }]
    }));
    first_payload["turnState"] = json!("turn_state_account_a");
    first_payload["x-codex-installation-id"] = json!("client_installation_a");
    first_payload["client_metadata"] = json!({
        "x-codex-installation-id": "client_installation_a",
        "x-codex-turn-state": "turn_state_account_a"
    });
    websocket
        .send(Message::Text(first_payload.to_string().into()))
        .await
        .unwrap();
    let first = receive_response(&mut websocket).await;
    assert_eq!(first.last().unwrap()["response"]["id"], "resp_account_a");

    timeout(Duration::from_secs(2), async {
        loop {
            if state
                .services
                .session_affinity
                .lookup("resp_account_a", Utc::now())
                .await
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first response affinity should be recorded before removal");
    assert!(
        state
            .services
            .session_affinity
            .forget("resp_account_a")
            .await
    );

    assert!(
        state
            .services
            .account_pool
            .set_status("acct_primary", AccountStatus::Disabled)
            .await
    );
    let mut second_payload: Value =
        serde_json::from_str(&response_create_payload("second")).unwrap();
    second_payload["previous_response_id"] = json!("resp_account_a");
    second_payload["turnState"] = json!("turn_state_account_a");
    second_payload["turn_state"] = json!("turn_state_account_a_projection");
    second_payload["x-codex-turn-state"] = json!("turn_state_account_a_projection");
    second_payload["turnMetadata"] = json!("{malformed-account-a-metadata");
    second_payload["authorization"] = json!("Bearer leaked-account-a");
    second_payload["cookie"] = json!("session=leaked-account-a");
    second_payload["chatgpt-account-id"] = json!("chatgpt-primary");
    second_payload["account_id"] = json!("acct_primary");
    second_payload["conversation"] = json!("conversation_account_a");
    second_payload["x-codex-installation-id"] = json!("client_installation_a");
    second_payload["installation_id"] = json!("client_installation_a");
    second_payload["installationId"] = json!("client_installation_a");
    second_payload["client_metadata"] = json!({
        "safe": "preserved",
        "account_id": "acct_primary",
        "cookie": "session=leaked-account-a",
        "x-codex-installation-id": "client_installation_a",
        "installation_id": "client_installation_a",
        "installationId": "client_installation_a",
        "x-codex-turn-state": "turn_state_account_a",
        "x-codex-turn-metadata": json!({
            "installation_id": "client_installation_a",
            "account_id": "acct_primary",
            "authorization": "Bearer leaked-account-a",
            "turn_state": "turn_state_account_a",
            "conversation": "conversation_account_a",
            "session_id": "session_semantic"
        }).to_string()
    });
    websocket
        .send(Message::Text(second_payload.to_string().into()))
        .await
        .unwrap();
    let second = receive_response(&mut websocket).await;
    assert_eq!(second.last().unwrap()["response"]["id"], "resp_account_b");

    let mut third_payload: Value = serde_json::from_str(&response_create_payload("third")).unwrap();
    third_payload["previous_response_id"] = json!("resp_account_b");
    websocket
        .send(Message::Text(third_payload.to_string().into()))
        .await
        .unwrap();
    let third = receive_response(&mut websocket).await;
    assert_eq!(
        third.last().unwrap()["response"]["id"],
        "resp_account_b_recovered"
    );

    let (
        primary_headers,
        secondary_headers,
        retry_headers,
        initial_primary,
        cross_account_replay,
        continued_secondary,
        same_account_replay,
    ) = upstream.await.unwrap();
    assert_eq!(
        request_header(&primary_headers, "authorization"),
        Some("Bearer access-primary")
    );
    assert_eq!(
        request_header(&secondary_headers, "authorization"),
        Some("Bearer access-secondary")
    );
    assert_eq!(
        request_header(&secondary_headers, "chatgpt-account-id"),
        Some("chatgpt-secondary")
    );
    assert_eq!(
        request_header(&secondary_headers, "cookie"),
        Some("cf_clearance=secondary")
    );
    assert_eq!(
        request_header(&retry_headers, "authorization"),
        Some("Bearer access-secondary")
    );
    let primary_installation = request_header(&primary_headers, "x-codex-installation-id").unwrap();
    let secondary_installation =
        request_header(&secondary_headers, "x-codex-installation-id").unwrap();
    assert_ne!(primary_installation, secondary_installation);
    assert_eq!(
        request_header(&retry_headers, "x-codex-installation-id"),
        Some(secondary_installation)
    );
    assert!(request_header(&secondary_headers, "x-codex-turn-state").is_none());
    assert!(request_header(&secondary_headers, "x-codex-turn-metadata").is_none());

    assert_eq!(initial_primary["turnState"], "turn_state_account_a");
    assert!(cross_account_replay.get("previous_response_id").is_none());
    for key in [
        "turnState",
        "turn_state",
        "x-codex-turn-state",
        "turnMetadata",
        "authorization",
        "cookie",
        "chatgpt-account-id",
        "account_id",
        "conversation",
    ] {
        assert!(
            cross_account_replay.get(key).is_none(),
            "cross-account replay leaked top-level field {key}"
        );
    }
    let metadata = &cross_account_replay["client_metadata"];
    assert_eq!(metadata["safe"], "preserved");
    for key in ["account_id", "cookie", "x-codex-turn-state"] {
        assert!(
            metadata.get(key).is_none(),
            "cross-account replay leaked metadata field {key}"
        );
    }
    let scoped_turn_metadata: Value = serde_json::from_str(
        metadata["x-codex-turn-metadata"]
            .as_str()
            .expect("valid client metadata turnMetadata should be retained"),
    )
    .unwrap();
    assert_eq!(
        scoped_turn_metadata["installation_id"].as_str(),
        Some(secondary_installation)
    );
    assert_eq!(scoped_turn_metadata["session_id"], "session_semantic");
    for key in ["account_id", "authorization", "turn_state", "conversation"] {
        assert!(
            scoped_turn_metadata.get(key).is_none(),
            "cross-account replay leaked turnMetadata field {key}"
        );
    }
    for key in [
        "x-codex-installation-id",
        "installation_id",
        "installationId",
    ] {
        assert_eq!(
            cross_account_replay[key].as_str(),
            Some(secondary_installation),
            "top-level installation projection {key}"
        );
        assert_eq!(
            metadata[key].as_str(),
            Some(secondary_installation),
            "metadata installation projection {key}"
        );
    }

    let replay_input = cross_account_replay["input"].as_array().unwrap();
    assert_eq!(replay_input.len(), 6);
    assert_eq!(replay_input[0]["id"], "client_input_id");
    assert_eq!(
        replay_input[0]["encrypted_content"],
        "client_semantic_secret"
    );
    assert_eq!(
        replay_input[0]["tool_arguments"]["id"],
        "client_tool_argument_id"
    );
    assert!(replay_input[1].get("id").is_none());
    assert!(replay_input[1].get("encrypted_content").is_none());
    assert_eq!(
        replay_input[1]["summary"][0]["id"],
        "client_replayed_summary_id_a"
    );
    assert!(replay_input[2].get("id").is_none());
    assert!(replay_input[2].get("encrypted_content").is_none());
    assert_eq!(replay_input[2]["content"][0]["id"], "nested_content_id_a");
    assert!(replay_input[3].get("id").is_none());
    assert!(replay_input[3].get("encrypted_content").is_none());
    assert_eq!(replay_input[3]["summary"][0]["id"], "nested_summary_id_a");
    assert!(replay_input[4].get("id").is_none());
    assert_eq!(replay_input[4]["call_id"], "call_account_a");
    assert_eq!(replay_input[4]["caller"], "client");
    assert_eq!(
        replay_input[4]["arguments"],
        "{\"id\":\"tool_argument_id_a\"}"
    );
    assert_eq!(replay_input[5]["content"], "second");
    let cross_account_json = cross_account_replay.to_string();
    for forbidden in [
        "msg_account_a",
        "reasoning_account_a",
        "function_account_a",
        "compaction_account_a",
        "encrypted_message_a",
        "encrypted_reasoning_a",
        "encrypted_compaction_a",
        "client_replayed_reasoning_id_a",
        "client_replayed_reasoning_encrypted_a",
    ] {
        assert!(
            !cross_account_json.contains(forbidden),
            "cross-account replay leaked {forbidden}"
        );
    }

    assert_eq!(
        continued_secondary["previous_response_id"],
        "resp_account_b"
    );
    assert_eq!(continued_secondary["input"][0]["content"], "third");
    assert!(same_account_replay.get("previous_response_id").is_none());
    let same_account_json = same_account_replay.to_string();
    assert!(!same_account_json.contains("encrypted_message_a"));
    assert!(!same_account_json.contains("encrypted_reasoning_a"));
    assert!(!same_account_json.contains("client_replayed_reasoning_encrypted_a"));
    assert!(same_account_json.contains("encrypted_message_b"));
    assert!(same_account_json.contains("encrypted_reasoning_b"));
    assert!(!same_account_json.contains("msg_account_b"));
    assert!(!same_account_json.contains("reasoning_account_b"));
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
    send_upstream_response_with_output(
        websocket,
        response_id,
        text,
        vec![json!({
            "type": "message",
            "id": format!("assistant_output_{response_id}"),
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        })],
    )
    .await;
}

async fn send_upstream_response_with_output(
    websocket: &mut WebSocketStream<tokio::net::TcpStream>,
    response_id: &str,
    text: &str,
    output: Vec<Value>,
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
                "output": output,
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

fn request_header<'a>(
    headers: &'a tokio_tungstenite::tungstenite::http::HeaderMap,
    name: &str,
) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
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
