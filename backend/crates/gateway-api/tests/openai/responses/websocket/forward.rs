use std::collections::VecDeque;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::Duration;

use axum::http::{StatusCode, header::AUTHORIZATION};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError};
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, ProviderError, ProviderErrorKind,
};
use gateway_core::event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader};
use gateway_core::upstream::{OpaqueUpstreamValue, UpstreamSendState};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

use super::{AtomicFailureExecution, AtomicFailureTrace};
use crate::openai::{api_router, authenticated_client};

type TestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

// 全部使用合成响应事实和真实 API 路由，不读取账号、日志或请求转储。
struct TestServer(tokio::task::JoinHandle<()>);

impl Drop for TestServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn connect(
    trace: Arc<AtomicFailureTrace>,
    response_headers: Vec<ProviderResponseHeader>,
) -> (TestSocket, TestServer) {
    let execution = Arc::new(AtomicFailureExecution {
        client: authenticated_client("sk_ws_atomic"),
        trace,
        response_headers,
        fail_before_first_event: false,
    });
    let app = api_router(execution).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local API");
    let address = listener.local_addr().unwrap();
    let server = TestServer(tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve local API");
    }));
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTHORIZATION, "Bearer sk_ws_atomic".parse().unwrap());
    let (socket, response) = connect_async(request).await.expect("upgrade WebSocket");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    (socket, server)
}

async fn send_request(socket: &mut TestSocket) {
    socket
        .send(Message::Text(
            json!({"type": "response.create", "model": "model-a", "input": "synthetic"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
}

async fn next_event(socket: &mut TestSocket) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("event timeout")
        .expect("connection remains open")
        .expect("valid frame");
    let Message::Text(text) = frame else {
        panic!("expected a JSON text frame, got {frame:?}");
    };
    serde_json::from_str(&text).expect("JSON event")
}

async fn next_error(socket: &mut TestSocket) -> Value {
    let first = next_event(socket).await;
    let error = if first["type"] == "response.metadata" {
        next_event(socket).await
    } else {
        first
    };
    assert_eq!(error["type"], "error");
    error
}

async fn assert_no_duplicate_failure(socket: &mut TestSocket) {
    // 后续入站帧只会在本轮收敛后处理，比固定等待窗口更可靠地检查没有重复失败终态。
    socket.send(Message::Text("{".into())).await.unwrap();
    let error = next_event(socket).await;
    assert_eq!(error["error"]["code"], "invalid_json");
    assert_eq!(error["status"], 400);
    assert_eq!(error["headers"]["x-request-id"], error["request_id"]);
    assert_eq!(
        error["headers"]["x-gateway-request-id"],
        error["request_id"]
    );
}

async fn initial_error(error: EngineError, response_headers: Vec<ProviderResponseHeader>) -> Value {
    let trace = Arc::new(AtomicFailureTrace {
        initial_errors: Mutex::new(VecDeque::from([error])),
        ..AtomicFailureTrace::default()
    });
    let (mut socket, _server) = connect(Arc::clone(&trace), response_headers).await;
    send_request(&mut socket).await;
    let error = next_error(&mut socket).await;
    assert!(!trace.committed.load(Ordering::Acquire));
    assert!(trace.finalized.load(Ordering::Acquire));
    assert_eq!(error["request_id"], "req_ws_atomic");
    assert_eq!(error["headers"]["x-gateway-request-id"], "req_ws_atomic");
    assert_no_duplicate_failure(&mut socket).await;
    socket.close(None).await.unwrap();
    error
}

fn header(name: &str, value: &'static [u8]) -> ProviderResponseHeader {
    ProviderResponseHeader::new(name, Bytes::from_static(value))
}

fn upstream_failure(
    status: u16,
    body: &'static [u8],
    headers: Vec<ProviderResponseHeader>,
) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
        .with_status(status)
        .with_client_visible_upstream_response(
            ClientVisibleUpstreamResponse::new(status, None, Bytes::from_static(body))
                .with_headers(headers),
        )
}

#[tokio::test]
async fn initial_http_or_opening_failure_preserves_status_and_structured_error() {
    // 分类故意都用 Unavailable：不能把收到的 401/429/500 等按网关分类改成 502。
    for status in [302, 400, 401, 403, 429, 500, 502, 503] {
        let provider = upstream_failure(
            status,
            br#"{"error":{"message":"synthetic upstream marker","code":"custom_code","type":"custom_type"}}"#,
            vec![header("x-request-id", b"req_current"), header("retry-after", b"7")],
        )
        .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            "synthetic upstream marker",
            Some("custom_code".to_owned()),
            Some("custom_type".to_owned()),
        ));
        let error = initial_error(EngineError::Provider(provider), Vec::new()).await;
        assert_eq!(error["status"], status);
        assert_eq!(
            error["error"],
            json!({
                "message": "synthetic upstream marker", "code": "custom_code", "type": "custom_type",
            })
        );
        assert_eq!(error["headers"]["x-request-id"], "req_current");
        assert_eq!(error["headers"]["retry-after"], "7");
    }
}

#[tokio::test]
async fn non_structured_bodies_use_safe_fallback_without_losing_status_or_ids() {
    for body in [
        b"<html>synthetic-private-body-marker</html>".as_slice(),
        b"synthetic-private-body-marker",
        b"",
        br#""synthetic-private-body-marker""#,
        br#"{"error":"synthetic-private-body-marker"}"#,
        br#"{"detail":"synthetic-private-body-marker"}"#,
        br#"{"error":{"code":"synthetic-private-body-marker"}}"#,
    ] {
        let provider = upstream_failure(
            503,
            body,
            vec![header("x-request-id", b"req_body_fallback")],
        );
        let error = initial_error(EngineError::Provider(provider), Vec::new()).await;
        assert_eq!(error["status"], 503);
        assert_eq!(error["headers"]["x-request-id"], "req_body_fallback");
        assert_eq!(
            error["error"],
            json!({
                "message": "upstream service is unavailable",
                "code": "upstream_unavailable",
                "type": "server_error",
            })
        );
        assert!(!error.to_string().contains("synthetic-private-body-marker"));
    }
}

#[tokio::test]
async fn final_failure_headers_take_precedence_over_observed_opening_headers() {
    let provider = upstream_failure(403, b"", vec![header("x-oai-request-id", b"req_final")])
        .with_status(502)
        .with_upstream_request_id(OpaqueUpstreamValue::new("req_stale_fact"));
    let error = initial_error(
        EngineError::Provider(provider),
        vec![
            header("x-request-id", b"req_old_opening"),
            header("x-codex-turn-state", b"old-turn-state"),
        ],
    )
    .await;
    assert_eq!(error["status"], 403);
    assert_eq!(error["headers"]["x-oai-request-id"], "req_final");
    assert!(error["headers"].get("x-request-id").is_none());
    assert!(!error.to_string().contains("req_old_opening"));
    assert!(!error.to_string().contains("req_stale_fact"));
    assert!(!error.to_string().contains("old-turn-state"));
}

#[tokio::test]
async fn provider_failure_facts_survive_without_a_complete_http_response() {
    let provider = ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Ambiguous)
        .with_status(429)
        .with_upstream_request_id(OpaqueUpstreamValue::new("req_body_read_failed"));
    let error = initial_error(
        EngineError::Provider(provider),
        vec![
            header("x-request-id", b"req_old"),
            header("x-oai-request-id", b"req_old_alias"),
            header("x-codex-turn-state", b"observed-turn-state"),
        ],
    )
    .await;
    assert_eq!(error["status"], 429);
    assert_eq!(error["headers"]["x-request-id"], "req_body_read_failed");
    assert_eq!(
        error["headers"]["x-codex-turn-state"],
        "observed-turn-state"
    );
    assert!(error["headers"].get("x-oai-request-id").is_none());
    assert_eq!(error["error"]["message"], "upstream service is unavailable");
}

#[tokio::test]
async fn initial_error_headers_apply_the_response_security_boundary() {
    let provider = upstream_failure(
        502,
        b"",
        vec![
            header("X-Request-Id", b"req_safe"),
            header("x-gateway-request-id", b"upstream-cannot-claim-gateway-id"),
            header("x-future-header", b"first"),
            header("x-future-header", b"second"),
            header("cf-ray", b"synthetic-ray"),
            header("retry-after", b"11"),
            header("authorization", b"synthetic-private-header-marker"),
            header("set-cookie", b"synthetic-private-header-marker"),
            header("chatgpt-account-id", b"synthetic-private-header-marker"),
            header(
                "x-codex-installation-id",
                b"synthetic-private-header-marker",
            ),
            header("connection", b"x-private-hop"),
            header("x-private-hop", b"synthetic-private-header-marker"),
            header("sec-websocket-accept", b"synthetic-private-header-marker"),
            header("content-length", b"123"),
            header("content-type", b"text/html"),
            header("bad\0name", b"synthetic-private-header-marker"),
            header("x-bad-value", b"synthetic-private-header-marker\r\n"),
            header("x-non-utf8", b"\xff"),
        ],
    );
    let error = initial_error(EngineError::Provider(provider), Vec::new()).await;
    assert_eq!(
        error["headers"],
        json!({
            "x-request-id": "req_safe",
            "x-gateway-request-id": "req_ws_atomic",
            "x-future-header": "second",
            "cf-ray": "synthetic-ray",
            "retry-after": "11",
        })
    );
}

#[tokio::test]
async fn initial_error_without_upstream_facts_keeps_the_local_contract() {
    for (error, status, code) in [
        (EngineError::Deadline, 504, "request_timeout"),
        (EngineError::EmptyRoutingPlan, 503, "no_available_provider"),
        (
            EngineError::Provider(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::NotSent,
            )),
            502,
            "upstream_unavailable",
        ),
    ] {
        let error = initial_error(error, Vec::new()).await;
        assert_eq!(error["status"], status);
        assert_eq!(error["error"]["code"], code);
        assert_eq!(error["headers"]["x-request-id"], "req_ws_atomic");
    }
}

#[tokio::test]
async fn initial_error_never_uses_a_success_or_upgrade_status_as_an_error_status() {
    for status in [0, 99, 101, 200, 204] {
        let error = initial_error(
            EngineError::Provider(upstream_failure(status, b"", Vec::new())),
            Vec::new(),
        )
        .await;
        assert_eq!(error["status"], 502);
    }
}

#[tokio::test]
async fn opening_ids_stay_in_metadata_but_do_not_identify_initial_failures() {
    for (failure, status) in [
        (EngineError::Deadline, 504),
        (
            EngineError::Provider(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::Ambiguous,
            )),
            502,
        ),
    ] {
        let trace = Arc::new(AtomicFailureTrace {
            initial_errors: Mutex::new(VecDeque::from([failure])),
            ..AtomicFailureTrace::default()
        });
        let (mut socket, _server) = connect(
            trace,
            vec![
                header("x-request-id", b"req_old_opening"),
                header("x-oai-request-id", b"req_observed"),
                header("x-codex-turn-state", b"observed-turn-state"),
                header("x-future-header", b"future-value"),
            ],
        )
        .await;
        send_request(&mut socket).await;
        // metadata 仍交付原会话观测；opening ID 不能因此成为当前失败的请求身份。
        assert_eq!(
            next_event(&mut socket).await,
            json!({
                "type": "response.metadata",
                "headers": {
                    "x-request-id": "req_old_opening",
                    "x-oai-request-id": "req_observed",
                    "x-codex-turn-state": "observed-turn-state",
                    "x-future-header": "future-value",
                },
            })
        );
        let error = next_event(&mut socket).await;
        assert_eq!(error["type"], "error");
        assert_eq!(error["status"], status);
        assert_eq!(error["request_id"], "req_ws_atomic");
        assert_eq!(
            error["headers"],
            json!({
                "x-request-id": "req_ws_atomic",
                "x-gateway-request-id": "req_ws_atomic",
                "x-codex-turn-state": "observed-turn-state",
                "x-future-header": "future-value",
            })
        );
        assert_no_duplicate_failure(&mut socket).await;
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn errors_on_a_reused_connection_keep_their_own_upstream_ids() {
    let errors = ["req_first", "req_second"].map(|request_id| {
        EngineError::Provider(
            upstream_failure(502, b"", Vec::new())
                .with_upstream_request_id(OpaqueUpstreamValue::new(request_id)),
        )
    });
    let trace = Arc::new(AtomicFailureTrace {
        initial_errors: Mutex::new(VecDeque::from(errors)),
        ..AtomicFailureTrace::default()
    });
    let (mut socket, _server) = connect(trace, Vec::new()).await;
    for request_id in ["req_first", "req_second"] {
        send_request(&mut socket).await;
        let error = next_error(&mut socket).await;
        assert_eq!(error["headers"]["x-request-id"], request_id);
        assert_eq!(error["headers"]["x-gateway-request-id"], "req_ws_atomic");
    }
    assert_no_duplicate_failure(&mut socket).await;
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn deliverable_business_failures_are_not_rewritten_or_followed_by_a_second_error() {
    for raw in [
        json!({
            "type": "error",
            "status": 429,
            "headers": {"x-request-id": "req_raw"},
            "error": {"message": "raw marker", "type": "custom_type", "code": "custom_code"},
            "future": {"keep": true},
        }),
        json!({
            "type": "response.failed",
            "response": {
                "id": "resp_raw",
                "status": "failed",
                "error": {"message": "raw marker", "code": "custom_code"},
            },
            "future": {"keep": true},
        }),
    ] {
        let event = ProviderEvent::wire(
            ProtocolWireEvent::json(
                "openai",
                Some(raw["type"].as_str().unwrap().to_owned()),
                raw.clone(),
            )
            .unwrap(),
        );
        let trace = Arc::new(AtomicFailureTrace {
            first_batch: Mutex::new(Some(
                CoordinatedEvent::try_batch(vec![event], CommitRequirement::CommitBeforeDelivery)
                    .unwrap(),
            )),
            ..AtomicFailureTrace::default()
        });
        let (mut socket, _server) = connect(Arc::clone(&trace), Vec::new()).await;
        send_request(&mut socket).await;
        assert_eq!(next_event(&mut socket).await["type"], "response.metadata");
        assert_eq!(next_event(&mut socket).await, raw);
        assert_no_duplicate_failure(&mut socket).await;
        assert!(trace.committed.load(Ordering::Acquire));
        assert!(trace.finalized.load(Ordering::Acquire));
        assert_eq!(trace.next_calls.load(Ordering::Acquire), 2);
        socket.close(None).await.unwrap();
    }
}
