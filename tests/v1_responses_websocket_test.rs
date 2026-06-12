use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::{SinkExt, StreamExt};
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_tungstenite::{
    accept_async, accept_hdr_async,
    tungstenite::{
        handshake::server::{Request as WsRequest, Response as WsResponse},
        Message,
    },
};
use tower::ServiceExt;

use codex_proxy_rs::codex::accounts::repository::AccountRepository;

mod common;

use common::{
    response_text,
    upstream::{
        build_imported_app, build_imported_app_with_accounts,
        build_imported_app_with_accounts_and_token_refresher, ImportAccount, StaticTokenRefresher,
    },
};

const WEBSOCKET_COMPLETED_RESPONSE: &str =
    include_str!("fixtures/v1_responses_websocket_completed.json");
const WEBSOCKET_HISTORY_RATE_LIMITED: &str =
    include_str!("fixtures/v1_responses_websocket_history_rate_limited.json");
const WEBSOCKET_RATE_LIMITED: &str =
    include_str!("fixtures/v1_responses_websocket_rate_limited.json");
const WEBSOCKET_TOKEN_REVOKED: &str =
    include_str!("fixtures/v1_responses_websocket_token_revoked.json");
const WEBSOCKET_FIRST_ACCOUNT_LIMITED: &str =
    include_str!("fixtures/v1_responses_websocket_first_account_limited.json");
const WEBSOCKET_SECOND_ACCOUNT_LIMITED: &str =
    include_str!("fixtures/v1_responses_websocket_second_account_limited.json");

#[tokio::test]
async fn v1_responses_should_use_websocket_for_previous_response_id_streaming() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        request_tx.send(request).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_route_ws", 8, 5).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_prev"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert!(body.contains("event: response.completed"));
    assert!(body.contains("\"id\":\"resp_route_ws\""));
    let request = request_rx.await.unwrap();
    assert_eq!(request["type"], "response.create");
    assert_eq!(request["previous_response_id"], "resp_prev");
    server.await.unwrap();
}

#[tokio::test]
async fn v1_responses_previous_response_id_websocket_429_should_not_retry_different_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .header("x-request-id", "req_ws_history_429")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_prev"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    server.await.unwrap();
    let account_b_usage =
        sqlx::query_as::<_, (i64,)>("select count(*) from account_usage where account_id = ?")
            .bind("acct_b")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(account_b_usage.0, 0);
}

#[tokio::test]
async fn v1_responses_non_stream_previous_response_id_websocket_429_should_not_retry_different_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .header("x-request-id", "req_ws_history_429_non_stream")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"previous_response_id":"resp_prev"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    server.await.unwrap();
    let account_b_usage =
        sqlx::query_as::<_, (i64,)>("select count(*) from account_usage where account_id = ?")
            .bind("acct_b")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(account_b_usage.0, 0);
}

#[tokio::test]
async fn v1_responses_websocket_without_history_should_fallback_and_refresh_fallback_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(30),
            WEBSOCKET_RATE_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-b",
            401,
            "Unauthorized",
            None,
            WEBSOCKET_TOKEN_REVOKED,
        )
        .await;
        accept_successful_websocket_response(&listener, "Bearer refreshed-b", "resp_ws_fallback")
            .await
    });
    let imported = build_imported_app_with_accounts_and_token_refresher(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
        StaticTokenRefresher {
            access_token: "refreshed-b".to_string(),
            refresh_token: None,
        },
    )
    .await;

    let response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"id\":\"resp_ws_fallback\""));
    let websocket_request = server.await.unwrap();
    assert_eq!(websocket_request["type"], "response.create");
    assert!(websocket_request.get("previous_response_id").is_none());
    let repo = AccountRepository::new(imported.pool.clone(), imported.secret_box);
    let account_b = repo.get("acct_b").await.unwrap().unwrap();
    assert_eq!(account_b.access_token.expose_secret(), "refreshed-b");
    assert_eq!(
        account_b.refresh_token.unwrap().expose_secret(),
        "refresh-b"
    );
    let usage_a: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_a")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage_a, (1, 0, 0));
    let usage_b: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_b")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage_b, (1, 3, 1));
}

#[tokio::test]
async fn v1_responses_websocket_without_history_should_return_429_when_fallback_accounts_exhausted()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(11),
            WEBSOCKET_FIRST_ACCOUNT_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-b",
            429,
            "Too Many Requests",
            Some(22),
            WEBSOCKET_SECOND_ACCOUNT_LIMITED,
        )
        .await;
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    server.await.unwrap();
    let usage_a: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_a")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(usage_a.0, 1);
    let usage_b: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_b")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(usage_b.0, 1);
}

async fn reject_next_websocket_upgrade(
    listener: &TcpListener,
    expected_authorization: &str,
    status: u16,
    reason: &str,
    retry_after_seconds: Option<u64>,
    body: &str,
) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let request = read_http_upgrade_request(&mut stream).await;
    assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
    assert!(
        request.contains(&format!("authorization: {expected_authorization}")),
        "unexpected websocket authorization header in request:\n{request}"
    );
    let retry_after = retry_after_seconds
        .map(|seconds| format!("retry-after: {seconds}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n{retry_after}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn accept_successful_websocket_response(
    listener: &TcpListener,
    expected_authorization: &str,
    response_id: &str,
) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let expected_authorization = expected_authorization.to_string();
    let mut websocket =
        accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
            assert_eq!(
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some(expected_authorization.as_str())
            );
            Ok(response)
        })
        .await
        .unwrap();
    let message = websocket.next().await.unwrap().unwrap();
    let request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
    websocket
        .send(Message::Text(
            websocket_completed_response(response_id, 3, 1).into(),
        ))
        .await
        .unwrap();
    websocket.close(None).await.unwrap();
    request
}

fn websocket_completed_response(
    response_id: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> String {
    let mut value: Value = serde_json::from_str(WEBSOCKET_COMPLETED_RESPONSE).unwrap();
    value["response"]["id"] = Value::String(response_id.to_string());
    value["response"]["usage"]["input_tokens"] = json!(input_tokens);
    value["response"]["usage"]["output_tokens"] = json!(output_tokens);
    value.to_string()
}

async fn read_http_upgrade_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).unwrap()
}
