//! Live HTTP 入参与鉴权验证

mod websocket;

use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use futures::future::BoxFuture;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionRequestMetadata, ExecutionService,
    StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
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
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "live test captured the provider endpoint request",
            ))
        })
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
