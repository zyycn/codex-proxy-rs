//! Live HTTP 入参与鉴权验证

mod websocket;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::{
    body::{Body, Bytes, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use futures::future::BoxFuture;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionRequestMetadata, ExecutionService,
    ExecutionSession, StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::engine::{CoordinatedEvent, EngineError, ModelRequestId};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader};
use gateway_core::operation::Operation;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::api_router;
use super::authenticated_client;
use super::models::ModelsExecution;

const LIVE_KEY: &str = "sk_live_test";

/// 捕获 start_provider_endpoint 请求的执行器；语音引导路径的行为断言入口。
#[derive(Clone, Default)]
struct LiveCapture {
    captured: Arc<Mutex<Option<StartProviderExecution>>>,
    gateway: Option<Arc<dyn gateway_core::live::LiveGateway>>,
    response_headers: Option<Vec<ProviderResponseHeader>>,
}

impl LiveCapture {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn operation(&self) -> Option<(Operation, ExecutionRequestMetadata)> {
        let captured = self.captured.lock().expect("capture").take()?;
        Some((captured.operation, captured.metadata))
    }
}

impl ExecutionService for LiveCapture {
    fn live_gateway(&self) -> Option<Arc<dyn gateway_core::live::LiveGateway>> {
        self.gateway.clone()
    }

    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        if plaintext == LIVE_KEY {
            Ok(authenticated_client(LIVE_KEY))
        } else {
            Err(ClientAuthenticationError::InvalidKey)
        }
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<gateway_core::routing::PublicModelId> {
        Vec::new()
    }

    fn contains_public_model(
        &self,
        _: &AuthenticatedClient,
        _: &gateway_core::routing::PublicModelId,
    ) -> bool {
        false
    }

    fn start(&self, _: StartExecution) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "live test must not start a response",
            ))
        })
    }

    fn start_provider_endpoint(
        &self,
        request: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        *self.captured.lock().expect("capture") = Some(request);
        Box::pin(async move {
            if let Some(headers) = &self.response_headers {
                return Ok(StartedExecution {
                    request_id: ModelRequestId::new("req_live_headers").unwrap(),
                    created_at: SystemTime::now(),
                    stream: false,
                    session: Box::new(LiveSession {
                        headers: headers.clone(),
                        finalized: AtomicBool::new(false),
                    }),
                });
            }
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "live test captured the provider endpoint request",
            ))
        })
    }
}

struct LiveSession {
    headers: Vec<ProviderResponseHeader>,
    finalized: AtomicBool,
}

impl ExecutionSession for LiveSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async { unreachable!("Live bootstrap is buffered") })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async {
            Ok(vec![ProviderEvent::wire(
                ProtocolWireEvent::raw_http_body("openai", Bytes::from_static(b"v=0\r\n")).unwrap(),
            )])
        })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &self.headers
    }

    fn response_status_code(&self) -> Option<u16> {
        Some(201)
    }

    fn commit_downstream(&mut self, _: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.finalized.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn is_finalized(&self) -> bool {
        self.finalized.load(Ordering::Acquire)
    }

    fn record_client_status(&mut self, _: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async { Ok(()) })
    }

    fn cancel(&self) {
        self.finalized.store(true, Ordering::Release);
    }

    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.cancel() })
    }
}

fn live_response_headers() -> Vec<ProviderResponseHeader> {
    [
        ("content-type", b"application/sdp".as_slice()),
        ("location", b"https://api.openai.com/v1/live/call_headers"),
        ("x-test-response", b"first"),
        ("x-test-response", b"\x80second"),
        ("set-cookie", b"private=1"),
        ("openai-organization", b"private-org"),
        ("connection", b"x-test-hop"),
        ("x-test-hop", b"private"),
    ]
    .into_iter()
    .map(|(name, value)| ProviderResponseHeader::new(name, Bytes::from_static(value)))
    .collect()
}

struct HangupGateway;

impl gateway_core::live::LiveGateway for HangupGateway {
    fn open_sideband<'a>(
        &'a self,
        _: gateway_core::live::LiveSidebandRequest<'a>,
    ) -> BoxFuture<'a, Result<gateway_core::live::LiveRelay, gateway_core::live::LiveGatewayError>>
    {
        Box::pin(async { unreachable!("hangup only") })
    }

    fn hangup<'a>(
        &'a self,
        request: gateway_core::live::LiveHangupRequest<'a>,
    ) -> BoxFuture<
        'a,
        Result<gateway_core::live::LiveCallOutcome, gateway_core::live::LiveGatewayError>,
    > {
        Box::pin(async move {
            assert_eq!(request.call_id, "call_headers");
            assert_eq!(
                request
                    .protocol_headers
                    .iter()
                    .filter(|header| header.name() == "x-test-business")
                    .map(|header| header.value())
                    .collect::<Vec<_>>(),
                [b"first".as_slice(), b"\x80second".as_slice()]
            );
            assert!(
                request
                    .protocol_headers
                    .iter()
                    .any(|header| header.name() == "voice-session-id"
                        && header.value().as_ref() == b"voice-test")
            );
            assert!(request.protocol_headers.iter().all(|header| {
                !["authorization", "connection", "x-test-hop"].contains(&header.name())
            }));
            Ok(gateway_core::live::LiveCallOutcome {
                status: 200,
                headers: live_response_headers(),
                body: Bytes::new(),
            })
        })
    }
}

#[tokio::test]
async fn live_hangup_should_preserve_business_header_bytes_in_both_directions() {
    let capture = Arc::new(LiveCapture {
        gateway: Some(Arc::new(HangupGateway)),
        ..LiveCapture::default()
    });
    let response = api_router(capture)
        .await
        .oneshot(
            Request::post("/v1/realtime/calls/call_headers/hangup")
                .header(AUTHORIZATION, format!("Bearer {LIVE_KEY}"))
                .header("voice-session-id", "voice-test")
                .header("x-test-business", "first")
                .header(
                    "x-test-business",
                    axum::http::HeaderValue::from_bytes(b"\x80second").unwrap(),
                )
                .header("connection", "x-test-hop")
                .header("x-test-hop", "private")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get_all("x-test-response")
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>(),
        [b"first".as_slice(), b"\x80second".as_slice()]
    );
    for name in [
        "set-cookie",
        "openai-organization",
        "connection",
        "x-test-hop",
    ] {
        assert!(!response.headers().contains_key(name));
    }
}

#[tokio::test]
async fn live_response_should_preserve_business_headers_and_rewrite_realtime_location() {
    let capture = Arc::new(LiveCapture {
        response_headers: Some(live_response_headers()),
        ..LiveCapture::default()
    });
    for path in ["/v1/live", "/v1/realtime/calls"] {
        let response = api_router(capture.clone())
            .await
            .oneshot(
                Request::post(path)
                    .header(AUTHORIZATION, format!("Bearer {LIVE_KEY}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sdp":"v=0"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()["content-type"], "application/sdp");
        let location = if path == "/v1/live" {
            "https://api.openai.com/v1/live/call_headers"
        } else {
            "/v1/realtime/calls/call_headers"
        };
        assert_eq!(response.headers()["location"], location);
        assert_eq!(
            response
                .headers()
                .get_all("x-test-response")
                .iter()
                .map(|value| value.as_bytes())
                .collect::<Vec<_>>(),
            [b"first".as_slice(), b"\x80second".as_slice()]
        );
        for name in [
            "set-cookie",
            "openai-organization",
            "connection",
            "x-test-hop",
        ] {
            assert!(!response.headers().contains_key(name));
        }
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            b"v=0\r\n".as_slice()
        );
    }
}

async fn capture_live_call(
    body: Body,
    content_type: &str,
) -> (StatusCode, Option<(Operation, ExecutionRequestMetadata)>) {
    let capture = LiveCapture::new();
    let response = api_router(Arc::clone(&capture) as Arc<dyn ExecutionService>)
        .await
        .oneshot(
            Request::post("/v1/live")
                .header(AUTHORIZATION, format!("Bearer {LIVE_KEY}"))
                .header("content-type", content_type)
                .body(body)
                .expect("build live request"),
        )
        .await
        .expect("route live request");
    (response.status(), capture.operation())
}

#[tokio::test]
async fn live_call_requires_a_client_key() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(
            Request::post("/v1/live")
                .header("content-type", "application/json")
                .body(Body::from(json!({"sdp": "v=0"}).to_string()))
                .expect("build live request"),
        )
        .await
        .expect("route live request");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn json_call_is_forwarded_to_the_realtime_calls_endpoint_with_rewritten_model() {
    let (status, captured) = capture_live_call(
        Body::from(
            json!({
                "sdp": "v=0\r\no=- 1 1 IN IP4 0.0.0.0",
                "session": {"model": "gpt-realtime", "voice": "alloy"}
            })
            .to_string(),
        ),
        "application/json",
    )
    .await;
    assert!(status.is_server_error() || status.is_client_error());
    let (operation, metadata) = captured.expect("captured provider endpoint request");
    let Operation::ProviderHttp(request) = operation else {
        panic!("expected provider http operation");
    };
    assert_eq!(request.endpoint(), "realtime-calls");
    assert_eq!(
        request.query(),
        Some("intent=quicksilver&architecture=avas")
    );
    let body: Value = serde_json::from_slice(request.payload().body()).expect("json body");
    assert!(body["sdp"].as_str().expect("sdp").starts_with("v=0"));
    assert_eq!(body["session"]["model"], "gpt-live-1-codex");
    assert_eq!(body["session"]["voice"], "alloy");
    let header_names = request
        .headers()
        .iter()
        .map(|header| header.name().to_owned())
        .collect::<Vec<_>>();
    assert!(header_names.contains(&"content-type".to_owned()));
    assert!(!header_names.contains(&"authorization".to_owned()));
    assert!(!header_names.contains(&"x-api-key".to_owned()));
    assert_eq!(metadata.endpoint, "/v1/live");
    assert!(!metadata.stream);
}

#[tokio::test]
async fn sdp_text_is_wrapped_into_the_json_call_request() {
    let (status, captured) = capture_live_call(
        Body::from("v=0\r\no=- 1 1 IN IP4 0.0.0.0"),
        "application/sdp",
    )
    .await;
    assert!(status.is_server_error() || status.is_client_error());
    let (operation, _) = captured.expect("captured provider endpoint request");
    let Operation::ProviderHttp(request) = operation else {
        panic!("expected provider http operation");
    };
    let body: Value = serde_json::from_slice(request.payload().body()).expect("json body");
    assert!(body["sdp"].as_str().is_some());
    assert!(body.get("session").is_none());
}

#[tokio::test]
async fn multipart_call_recombines_sdp_and_session_fields() {
    let boundary = "live_test_boundary";
    let multipart = format!(
        "--{boundary}\r\ncontent-disposition: form-data; name=\"sdp\"\r\n\r\nv=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n--{boundary}\r\ncontent-disposition: form-data; name=\"session\"\r\ncontent-type: application/json\r\n\r\n{{\"model\":\"gpt-realtime\"}}\r\n--{boundary}--\r\n"
    );
    let (status, captured) = capture_live_call(
        Body::from(multipart),
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert!(status.is_server_error() || status.is_client_error());
    let (operation, _) = captured.expect("captured provider endpoint request");
    let Operation::ProviderHttp(request) = operation else {
        panic!("expected provider http operation");
    };
    let body: Value = serde_json::from_slice(request.payload().body()).expect("json body");
    assert!(body["sdp"].as_str().is_some());
    // multipart 的 session.model 同样走提取与改写链路。
    assert_eq!(body["session"]["model"], "gpt-live-1-codex");
}

#[tokio::test]
async fn client_identity_headers_never_reach_the_provider_operation() {
    let (status, captured) = capture_live_call(
        Body::from(json!({"sdp": "v=0"}).to_string()),
        "application/json",
    )
    .await;
    assert!(status.is_server_error() || status.is_client_error());
    let (operation, _) = captured.expect("captured provider endpoint request");
    let Operation::ProviderHttp(request) = operation else {
        panic!("expected provider http operation");
    };
    let header_names = request
        .headers()
        .iter()
        .map(|header| header.name().to_ascii_lowercase())
        .collect::<Vec<_>>();
    assert!(!header_names.contains(&"authorization".to_owned()));
    assert!(!header_names.contains(&"cookie".to_owned()));
    assert!(!header_names.contains(&"host".to_owned()));
}

#[tokio::test]
async fn sideband_requires_a_websocket_upgrade() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(
            Request::get("/v1/live/call_1")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build sideband request"),
        )
        .await
        .expect("route sideband request");
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn realtime_capability_stubs_return_501() {
    for (method, path) in [
        ("POST", "/v1/realtime/sessions"),
        ("POST", "/v1/realtime/client_secrets"),
        ("POST", "/v1/realtime/transcription_sessions"),
        ("POST", "/v1/realtime/translations"),
        ("POST", "/v1/realtime/calls/call_1/accept"),
    ] {
        let response = api_router(ModelsExecution::new())
            .await
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(AUTHORIZATION, "Bearer sk_models_test")
                    .body(Body::empty())
                    .expect("build realtime stub request"),
            )
            .await
            .expect("route realtime stub request");
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED, "{path}");
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            body["error"]["code"], "realtime_capability_not_supported",
            "{path}"
        );
    }
}

#[tokio::test]
async fn direct_realtime_websocket_is_reported_unsupported() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(
            Request::get("/v1/realtime?model=gpt-realtime")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build realtime request"),
        )
        .await
        .expect("route realtime request");
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn hangup_without_a_live_gateway_is_reported_unsupported() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(
            Request::post("/v1/realtime/calls/call_unknown/hangup")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .expect("build hangup request"),
        )
        .await
        .expect("route hangup request");
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn live_post_only_accepts_declared_methods() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(
            Request::put("/v1/live")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build live put request"),
        )
        .await
        .expect("route live put request");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn live_should_preserve_business_headers_without_downstream_identity_or_transport() {
    let capture = LiveCapture::new();
    api_router(Arc::clone(&capture) as Arc<dyn ExecutionService>)
        .await
        .oneshot(
            Request::post("/v1/live")
                .header(AUTHORIZATION, format!("Bearer {LIVE_KEY}"))
                .header("content-type", "application/json")
                .header("originator", "downstream-client")
                .header("openai-organization", "downstream-org")
                .header("openai-project", "downstream-project")
                .header("x-oai-attestation", "downstream-attestation")
                .header("voice-session-id", "voice-test")
                .header("x-test-business", "first")
                .header(
                    "x-test-business",
                    axum::http::HeaderValue::from_bytes(b"\x80second").unwrap(),
                )
                .header("connection", "x-hop-test")
                .header("x-hop-test", "hop-only")
                .body(Body::from(json!({"sdp":"v=0"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (Operation::ProviderHttp(request), _) = capture.operation().unwrap() else {
        panic!("operation");
    };
    let leaked: Vec<_> = request
        .headers()
        .iter()
        .map(|h| h.name())
        .filter(|h| {
            [
                "originator",
                "openai-organization",
                "openai-project",
                "x-oai-attestation",
                "authorization",
                "connection",
                "x-hop-test",
            ]
            .contains(h)
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "downstream identity headers reach upstream operation: {leaked:?}"
    );
    assert!(
        request
            .headers()
            .iter()
            .any(|header| header.name() == "voice-session-id"
                && header.value().as_ref() == b"voice-test")
    );
    let values: Vec<_> = request
        .headers()
        .iter()
        .filter(|header| header.name() == "x-test-business")
        .map(|header| header.value())
        .collect();
    assert_eq!(values, [b"first".as_slice(), b"\x80second".as_slice()]);
}
