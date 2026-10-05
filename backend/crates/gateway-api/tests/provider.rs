//! Provider 原生入口的请求捕获、协议传递与响应交付测试

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header::AUTHORIZATION},
};
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::{
    engine::execution::{
        AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
        StartExecution, StartProviderExecution, StartedExecution,
    },
    engine::{CoordinatedEvent, EngineError, ModelRequestId},
    error::{GatewayError, GatewayErrorKind},
    event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader},
    operation::{Operation, ProviderHttpMethod},
    routing::PublicModelId,
};
use tower::ServiceExt as _;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CapturedOperation {
    CountTokens {
        provider: String,
        model: String,
        protocol: String,
        endpoint: String,
        body: Bytes,
    },
    Http {
        provider: String,
        endpoint_id: String,
        method: ProviderHttpMethod,
        query: Option<String>,
        protocol: String,
        route: String,
        headers: Vec<(String, Bytes)>,
        body: Bytes,
    },
}

struct ProviderExecution {
    client: AuthenticatedClient,
    captured: Mutex<Vec<CapturedOperation>>,
}

impl ProviderExecution {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            client: crate::openai::authenticated_client_for_provider(
                "sk_provider_operation_test",
                "local-echo",
            ),
            captured: Mutex::new(Vec::new()),
        })
    }

    fn captured(&self) -> Vec<CapturedOperation> {
        self.captured.lock().expect("capture lock").clone()
    }
}

impl ExecutionService for ProviderExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        (plaintext == "sk_provider_operation_test")
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
                "provider operation test must use a provider endpoint",
            ))
        })
    }

    fn start_provider_endpoint(
        &self,
        request: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let (captured, response, status) = match request.operation {
                Operation::CountTokens(count) => {
                    let model = request
                        .upstream_model
                        .expect("token count model binding")
                        .as_str()
                        .to_owned();
                    (
                        CapturedOperation::CountTokens {
                            provider: request.provider.as_str().to_owned(),
                            model,
                            protocol: request.metadata.protocol,
                            endpoint: request.metadata.endpoint,
                            body: count.payload().body().clone(),
                        },
                        SessionResponse::Json(Bytes::from_static(
                            br#"{ "input_tokens": 2, "tokenizer":"local_whitespace_v1" }"#,
                        )),
                        StatusCode::OK.as_u16(),
                    )
                }
                Operation::ProviderHttp(http) => {
                    let status = if http.endpoint() == "redirect-fixture" {
                        StatusCode::FOUND
                    } else {
                        StatusCode::OK
                    };
                    (
                        CapturedOperation::Http {
                            provider: request.provider.as_str().to_owned(),
                            endpoint_id: http.endpoint().to_owned(),
                            method: http.method(),
                            query: http.query().map(str::to_owned),
                            protocol: request.metadata.protocol,
                            route: request.metadata.endpoint,
                            headers: http
                                .headers()
                                .iter()
                                .map(|header| (header.name().to_owned(), header.value().clone()))
                                .collect(),
                            body: http.payload().body().clone(),
                        },
                        SessionResponse::Opaque(match http.endpoint() {
                            "oversized-fragment-fixture" => {
                                vec![Bytes::from(vec![0; 64 * 1024 + 1])]
                            }
                            "aggregate-overflow-fixture" => {
                                vec![Bytes::from_static(&[0; 64 * 1024]); 129]
                            }
                            "missing-body-fixture" => Vec::new(),
                            "empty-body-fixture" => vec![Bytes::new()],
                            _ => vec![
                                Bytes::from_static(b"\0raw-provider-"),
                                Bytes::from_static(b"body\xff"),
                            ],
                        }),
                        status.as_u16(),
                    )
                }
                _ => {
                    return Err(GatewayError::new(
                        GatewayErrorKind::Internal,
                        "unexpected provider operation",
                    ));
                }
            };
            self.captured.lock().expect("capture lock").push(captured);
            Ok(StartedExecution {
                request_id: ModelRequestId::new("req_provider_operation_test").expect("request ID"),
                created_at: SystemTime::now(),
                stream: false,
                session: Box::new(ProviderSession {
                    response: Some(response),
                    status,
                    finalized: AtomicBool::new(false),
                }),
            })
        })
    }
}

enum SessionResponse {
    Json(Bytes),
    Opaque(Vec<Bytes>),
}

struct ProviderSession {
    response: Option<SessionResponse>,
    status: u16,
    finalized: AtomicBool,
}

impl ExecutionSession for ProviderSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async { unreachable!("buffered provider response does not stream") })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            match self.response.take().expect("single response") {
                SessionResponse::Json(body) => Ok(vec![ProviderEvent::wire(
                    ProtocolWireEvent::raw_json("token-count", body).expect("JSON protocol"),
                )]),
                SessionResponse::Opaque(chunks) => Ok(chunks
                    .into_iter()
                    .map(|body| {
                        ProviderEvent::wire(
                            ProtocolWireEvent::raw_http_body("provider-http", body)
                                .expect("HTTP protocol"),
                        )
                    })
                    .collect()),
            }
        })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        static HEADERS: std::sync::LazyLock<Vec<ProviderResponseHeader>> =
            std::sync::LazyLock::new(|| {
                vec![
                    ProviderResponseHeader::new(
                        "content-type",
                        Bytes::from_static(b"application/x-provider"),
                    ),
                    ProviderResponseHeader::new("content-length", Bytes::from_static(b"9999")),
                    ProviderResponseHeader::new("set-cookie", Bytes::from_static(b"secret=1")),
                    ProviderResponseHeader::new(
                        "location",
                        Bytes::from_static(b"https://evil.invalid"),
                    ),
                    ProviderResponseHeader::new("connection", Bytes::from_static(b"x-hop")),
                    ProviderResponseHeader::new("x-hop", Bytes::from_static(b"secret")),
                    ProviderResponseHeader::new("x-provider-safe", Bytes::from_static(b"kept")),
                ]
            });
        HEADERS.as_slice()
    }

    fn response_status_code(&self) -> Option<u16> {
        Some(self.status)
    }

    fn commit_downstream(&mut self, _: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
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
async fn token_count_should_bind_provider_model_and_preserve_exact_json() {
    let execution = ProviderExecution::new();
    let request_body = Bytes::from_static(br#"{ "input":"one two", "future":9007199254740993 }"#);
    let response = crate::openai::api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/providers/local-echo/models/local-echo/count_tokens")
                .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                .body(Body::from(request_body.clone()))
                .expect("token count request"),
        )
        .await
        .expect("token count response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["content-length"], "56");
    assert_eq!(response.headers()["x-provider-safe"], "kept");
    assert!(!response.headers().contains_key("set-cookie"));
    assert!(!response.headers().contains_key("location"));
    assert!(!response.headers().contains_key("x-hop"));
    let body = to_bytes(response.into_body(), 1024).await.expect("body");
    assert_eq!(
        body.as_ref(),
        br#"{ "input_tokens": 2, "tokenizer":"local_whitespace_v1" }"#
    );
    assert_eq!(
        execution.captured(),
        vec![CapturedOperation::CountTokens {
            provider: "local-echo".to_owned(),
            model: "local-echo".to_owned(),
            protocol: "token-count".to_owned(),
            endpoint: "/v1/providers/local-echo/models/local-echo/count_tokens".to_owned(),
            body: request_body,
        }]
    );
}

#[tokio::test]
async fn provider_http_should_strip_sensitive_headers_and_preserve_opaque_bytes() {
    let execution = ProviderExecution::new();
    let request_body = Bytes::from_static(b"\x00input\xff");
    let response = crate::openai::api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/providers/local-echo/http/inspect?mode=future")
                .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                .header("x-api-key", "must-not-reach-plugin")
                .header("cookie", "must-not-reach-plugin=1")
                .header("connection", "x-hop-client")
                .header("x-hop-client", "must-not-reach-plugin")
                .header("x-safe-client", "kept")
                .body(Body::from(request_body.clone()))
                .expect("provider HTTP request"),
        )
        .await
        .expect("provider HTTP response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/x-provider");
    assert_eq!(response.headers()["content-length"], "19");
    assert!(!response.headers().contains_key("set-cookie"));
    assert!(!response.headers().contains_key("location"));
    let body = to_bytes(response.into_body(), 1024).await.expect("body");
    assert_eq!(body.as_ref(), b"\0raw-provider-body\xff");

    let captured = execution.captured();
    let [CapturedOperation::Http { headers, .. }] = captured.as_slice() else {
        panic!("HTTP operation captured");
    };
    assert!(
        headers
            .iter()
            .any(|(name, value)| { name == "x-safe-client" && value.as_ref() == b"kept" })
    );
    assert!(!headers.iter().any(|(name, _)| {
        matches!(
            name.as_str(),
            "authorization" | "x-api-key" | "cookie" | "connection" | "x-hop-client"
        )
    }));
    assert_eq!(
        captured[0],
        CapturedOperation::Http {
            provider: "local-echo".to_owned(),
            endpoint_id: "inspect".to_owned(),
            method: ProviderHttpMethod::Post,
            query: Some("mode=future".to_owned()),
            protocol: "provider-http".to_owned(),
            route: "/v1/providers/local-echo/http/inspect".to_owned(),
            headers: headers.clone(),
            body: request_body,
        }
    );
}

#[tokio::test]
async fn provider_http_should_reject_head_and_get_bodies_before_execution() {
    let execution = ProviderExecution::new();
    let router = crate::openai::api_router(execution.clone()).await;
    let head = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/providers/local-echo/http/inspect")
                .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                .body(Body::empty())
                .expect("HEAD request"),
        )
        .await
        .expect("HEAD response");
    assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);

    let get = router
        .oneshot(
            Request::get("/v1/providers/local-echo/http/inspect")
                .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                .body(Body::from("not-empty"))
                .expect("GET request"),
        )
        .await
        .expect("GET response");
    assert_eq!(get.status(), StatusCode::BAD_REQUEST);
    assert!(execution.captured().is_empty());
}

#[tokio::test]
async fn token_count_should_reject_invalid_json_before_execution() {
    let execution = ProviderExecution::new();
    let response = crate::openai::api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/providers/local-echo/models/local-echo/count_tokens")
                .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                .body(Body::from("{not-json"))
                .expect("token count request"),
        )
        .await
        .expect("token count response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(execution.captured().is_empty());
}

#[tokio::test]
async fn provider_http_should_fail_closed_on_non_success_success_envelope() {
    let execution = ProviderExecution::new();
    let response = crate::openai::api_router(execution)
        .await
        .oneshot(
            Request::get("/v1/providers/local-echo/http/redirect-fixture")
                .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                .body(Body::empty())
                .expect("provider HTTP request"),
        )
        .await
        .expect("provider HTTP response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(!response.headers().contains_key("location"));
}

#[tokio::test]
async fn provider_http_should_enforce_fragment_aggregate_and_body_presence_bounds() {
    let empty = crate::openai::api_router(ProviderExecution::new())
        .await
        .oneshot(
            Request::get("/v1/providers/local-echo/http/empty-body-fixture")
                .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                .body(Body::empty())
                .expect("empty provider HTTP request"),
        )
        .await
        .expect("empty provider HTTP response");
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(empty.headers()["content-length"], "0");
    assert!(
        to_bytes(empty.into_body(), 1)
            .await
            .expect("empty body")
            .is_empty()
    );

    for endpoint in [
        "oversized-fragment-fixture",
        "aggregate-overflow-fixture",
        "missing-body-fixture",
    ] {
        let execution = ProviderExecution::new();
        let response = crate::openai::api_router(execution)
            .await
            .oneshot(
                Request::get(format!("/v1/providers/local-echo/http/{endpoint}"))
                    .header(AUTHORIZATION, "Bearer sk_provider_operation_test")
                    .body(Body::empty())
                    .expect("provider HTTP request"),
            )
            .await
            .expect("provider HTTP response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "{endpoint}");
    }
}
