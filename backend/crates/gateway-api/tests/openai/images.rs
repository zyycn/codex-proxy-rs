use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::SystemTime;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ClientTransport, ExecutionService,
    ExecutionSession, StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::engine::{CoordinatedEvent, EngineError, ModelRequestId};
use gateway_core::error::{
    ClientVisibleUpstreamResponse, GatewayError, GatewayErrorKind, ProviderError, ProviderErrorKind,
};
use gateway_core::event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader};
use gateway_core::operation::{ImageRequestKind, Operation};
use gateway_core::routing::PublicModelId;
use gateway_core::upstream::UpstreamSendState;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{api_router, authenticated_client};

const IMAGE_RESPONSE: &[u8] =
    br#"{ "created": 1787212800, "data": [{"b64_json":"AAEC"}], "future": 9007199254740993 }"#;

#[derive(Debug, Clone, PartialEq)]
struct CapturedImageRequest {
    provider: String,
    endpoint: String,
    transport: ClientTransport,
    kind: ImageRequestKind,
    body: Bytes,
    context: Value,
}

struct ImageExecution {
    client: AuthenticatedClient,
    captured: Arc<Mutex<Vec<CapturedImageRequest>>>,
    committed_statuses: Arc<Mutex<Vec<u16>>>,
    fail_with_upstream_response: bool,
}

impl ImageExecution {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            client: authenticated_client("sk_images_test"),
            captured: Arc::new(Mutex::new(Vec::new())),
            committed_statuses: Arc::new(Mutex::new(Vec::new())),
            fail_with_upstream_response: false,
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            client: authenticated_client("sk_images_test"),
            captured: Arc::new(Mutex::new(Vec::new())),
            committed_statuses: Arc::new(Mutex::new(Vec::new())),
            fail_with_upstream_response: true,
        })
    }

    fn captured(&self) -> Vec<CapturedImageRequest> {
        self.captured.lock().expect("capture lock").clone()
    }

    fn committed_statuses(&self) -> Vec<u16> {
        self.committed_statuses
            .lock()
            .expect("committed status lock")
            .clone()
    }
}

impl ExecutionService for ImageExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        (plaintext == "sk_images_test")
            .then(|| self.client.clone())
            .ok_or(ClientAuthenticationError::InvalidKey)
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        Vec::new()
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, _: &PublicModelId) -> bool {
        false
    }

    fn start(&self, _: StartExecution) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "images test must use provider endpoint execution",
            ))
        })
    }

    fn start_provider_endpoint(
        &self,
        request: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let Operation::GenerateImage(image) = &request.operation else {
                return Err(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "images test received a non-image operation",
                ));
            };
            let payload = image.payload();
            self.captured
                .lock()
                .expect("capture lock")
                .push(CapturedImageRequest {
                    provider: request.provider.as_str().to_owned(),
                    endpoint: request.metadata.endpoint,
                    transport: request.metadata.transport,
                    kind: image.kind(),
                    body: payload.body().clone(),
                    context: Value::Object(payload.context().clone()),
                });
            Ok(StartedExecution {
                request_id: ModelRequestId::new("req_images_test").expect("request ID"),
                created_at: SystemTime::now(),
                stream: false,
                session: Box::new(ImageSession {
                    response: Some(Bytes::from_static(IMAGE_RESPONSE)),
                    response_headers: vec![ProviderResponseHeader::new(
                        "x-image-rate-limit",
                        Bytes::from_static(b"42"),
                    )],
                    committed_statuses: Arc::clone(&self.committed_statuses),
                    finalized: Arc::new(AtomicBool::new(false)),
                    fail_with_upstream_response: self.fail_with_upstream_response,
                }),
            })
        })
    }
}

struct ImageSession {
    response: Option<Bytes>,
    response_headers: Vec<ProviderResponseHeader>,
    committed_statuses: Arc<Mutex<Vec<u16>>>,
    finalized: Arc<AtomicBool>,
    fail_with_upstream_response: bool,
}

impl ExecutionSession for ImageSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async { unreachable!("buffered image delivery does not stream") })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            if self.fail_with_upstream_response {
                return Err(EngineError::Provider(
                    ProviderError::new(
                        ProviderErrorKind::InvalidRequest,
                        UpstreamSendState::Sent,
                    )
                    .with_status(StatusCode::UNPROCESSABLE_ENTITY.as_u16())
                    .with_client_visible_upstream_response(
                        ClientVisibleUpstreamResponse::new(
                            StatusCode::UNPROCESSABLE_ENTITY.as_u16(),
                            Some(b"application/problem+json".to_vec()),
                            Bytes::from_static(
                                br#"{ "error":{"message":"future image validation"}, "future":9007199254740993 }"#,
                            ),
                        )
                        .with_headers(vec![ProviderResponseHeader::new(
                            "x-future-image-error",
                            Bytes::from_static(b"preserved"),
                        )]),
                    ),
                ));
            }
            Ok(vec![ProviderEvent::wire(
                ProtocolWireEvent::raw_json(
                    "openai",
                    self.response.take().expect("single image response"),
                )
                .expect("OpenAI protocol"),
            )])
        })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &self.response_headers
    }

    fn response_status_code(&self) -> Option<u16> {
        Some(StatusCode::CREATED.as_u16())
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.committed_statuses
                .lock()
                .expect("committed status lock")
                .push(client_status_code.expect("image response status"));
            self.finalized.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn record_client_status(&mut self, _: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async { Ok(()) })
    }

    fn is_finalized(&self) -> bool {
        self.finalized.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.finalized.store(true, Ordering::Release);
    }

    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.finalized.store(true, Ordering::Release);
        })
    }
}

#[tokio::test]
async fn image_routes_should_not_decode_bodies_and_should_preserve_both_directions() {
    let execution = ImageExecution::new();
    let router = api_router(execution.clone()).await;
    let cases = [
        (
            "/v1/images/generations",
            ImageRequestKind::Generation,
            br#"{ "model":"gpt-image-future", "prompt":"a lighthouse", "future":9007199254740993 }"#.as_slice(),
        ),
        (
            "/v1/images/edits",
            ImageRequestKind::Edit,
            br#"{"model":"gpt-image-2","images":[{"image_url":"data:image/png;base64,AAEC"}],"prompt":"add fog","prompt":"duplicate stays raw"}"#.as_slice(),
        ),
    ];

    for (path, _, body) in &cases {
        let response = router
            .clone()
            .oneshot(
                Request::post(*path)
                    .header(AUTHORIZATION, "Bearer sk_images_test")
                    .header("content-type", "application/json")
                    .header("x-codex-image-turn-id", "turn_image_route")
                    .body(Body::from(body.to_vec()))
                    .expect("image request"),
            )
            .await
            .expect("image response");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get("x-image-rate-limit"),
            Some(&"42".parse().expect("header value"))
        );
        let response_body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read image response");
        assert_eq!(response_body.as_ref(), IMAGE_RESPONSE);
    }

    let captured = execution.captured();
    assert_eq!(captured.len(), cases.len());
    for (captured, (endpoint, kind, body)) in captured.iter().zip(cases.iter()) {
        assert_eq!(captured.provider, "openai");
        assert_eq!(captured.endpoint, *endpoint);
        assert_eq!(captured.transport, ClientTransport::HttpJson);
        assert_eq!(captured.kind, *kind);
        assert_eq!(captured.body.as_ref(), *body);
        assert_eq!(
            captured.context,
            json!({"image_turn_id": "turn_image_route"})
        );
    }
    assert_eq!(
        execution.committed_statuses(),
        vec![StatusCode::CREATED.as_u16(); cases.len()]
    );
}

#[tokio::test]
async fn image_route_should_return_the_exact_upstream_error_response() {
    let execution = ImageExecution::failing();
    let response = api_router(execution)
        .await
        .oneshot(
            Request::post("/v1/images/edits")
                .header(AUTHORIZATION, "Bearer sk_images_test")
                .header("content-type", "application/json")
                .body(Body::from(
                    br#"{ "model":"gpt-image-2", "future_invalid":9007199254740993 }"#.to_vec(),
                ))
                .expect("image request"),
        )
        .await
        .expect("image response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response.headers().get("content-type"),
        Some(&"application/problem+json".parse().expect("content type"))
    );
    assert_eq!(
        response.headers().get("x-future-image-error"),
        Some(&"preserved".parse().expect("header value"))
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read image response");
    assert_eq!(
        body.as_ref(),
        br#"{ "error":{"message":"future image validation"}, "future":9007199254740993 }"#
    );
}
