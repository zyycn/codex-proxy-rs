use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::engine::ModelRequestId;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ClientTransport, ExecutionService,
    StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::error::{
    ClientVisibleUpstreamResponse, GatewayError, GatewayErrorKind, ProviderError, ProviderErrorKind,
};
use gateway_core::event::ProviderResponseHeader;
use gateway_core::operation::Operation;
use gateway_core::routing::PublicModelId;
use gateway_core::upstream::UpstreamSendState;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::provider_endpoint::BufferedJsonSession;
use super::{api_router, authenticated_client};

const SEARCH_RESPONSE: &[u8] = br#"{ "encrypted_output":"ciphertext", "output":"search result", "results":[{"type":"text_result","ref_id":"turn0search0","future":9007199254740993}] }"#;

#[derive(Debug, Clone, PartialEq)]
struct CapturedSearchRequest {
    provider: String,
    endpoint: String,
    transport: ClientTransport,
    body: Bytes,
    context: Value,
}

struct SearchExecution {
    client: AuthenticatedClient,
    captured: Mutex<Vec<CapturedSearchRequest>>,
    committed_statuses: Arc<Mutex<Vec<u16>>>,
    fail_with_upstream_response: bool,
}

impl SearchExecution {
    fn new(fail_with_upstream_response: bool) -> Arc<Self> {
        Arc::new(Self {
            client: authenticated_client("sk_search_test"),
            captured: Mutex::new(Vec::new()),
            committed_statuses: Arc::new(Mutex::new(Vec::new())),
            fail_with_upstream_response,
        })
    }

    fn captured(&self) -> Vec<CapturedSearchRequest> {
        self.captured.lock().expect("capture lock").clone()
    }
}

impl ExecutionService for SearchExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        (plaintext == "sk_search_test")
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
                "search test must use provider endpoint execution",
            ))
        })
    }

    fn start_provider_endpoint(
        &self,
        request: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let Operation::Search(search) = &request.operation else {
                return Err(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "search test received a non-search operation",
                ));
            };
            let payload = search.payload();
            self.captured
                .lock()
                .expect("capture lock")
                .push(CapturedSearchRequest {
                    provider: request.provider.as_str().to_owned(),
                    endpoint: request.metadata.endpoint,
                    transport: request.metadata.transport,
                    body: payload.body().clone(),
                    context: Value::Object(payload.context().clone()),
                });
            let session = if self.fail_with_upstream_response {
                BufferedJsonSession::failure(
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
                                br#"{ "error":{"message":"future search validation"}, "future":9007199254740993 }"#,
                            ),
                        )
                        .with_headers(vec![ProviderResponseHeader::new(
                            "x-future-search-error",
                            Bytes::from_static(b"preserved"),
                        )]),
                    ),
                )
            } else {
                BufferedJsonSession::success(
                    Bytes::from_static(SEARCH_RESPONSE),
                    StatusCode::CREATED.as_u16(),
                    vec![ProviderResponseHeader::new(
                        "x-search-rate-limit",
                        Bytes::from_static(b"42"),
                    )],
                    Arc::clone(&self.committed_statuses),
                )
            };
            Ok(StartedExecution {
                request_id: ModelRequestId::new("req_search_test").expect("request ID"),
                created_at: SystemTime::now(),
                stream: false,
                session: Box::new(session),
            })
        })
    }
}

#[tokio::test]
async fn search_route_should_preserve_request_and_success_response_bytes() {
    let execution = SearchExecution::new(false);
    let request_body = br#"{ "id":"search-session", "model":"gpt-future", "commands":{"search_query":[{"q":"private query"}]}, "future":1, "future":2 }"#;
    let turn_metadata =
        r#"{"session_id":"session","thread_id":"thread","turn_id":"turn","future":true}"#;
    let response = api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/alpha/search")
                .header(AUTHORIZATION, "Bearer sk_search_test")
                .header("content-type", "application/json")
                .header("x-codex-turn-metadata", turn_metadata)
                .body(Body::from(request_body.to_vec()))
                .expect("search request"),
        )
        .await
        .expect("search response");

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers().get("x-search-rate-limit"),
        Some(&"42".parse().expect("header value"))
    );
    let response_body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read search response");
    assert_eq!(response_body.as_ref(), SEARCH_RESPONSE);

    let captured = execution.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].provider, "openai");
    assert_eq!(captured[0].endpoint, "/v1/alpha/search");
    assert_eq!(captured[0].transport, ClientTransport::HttpJson);
    assert_eq!(captured[0].body.as_ref(), request_body);
    assert_eq!(captured[0].context, json!({"turn_metadata": turn_metadata}));
    assert_eq!(
        *execution
            .committed_statuses
            .lock()
            .expect("committed status lock"),
        vec![StatusCode::CREATED.as_u16()]
    );
}

#[tokio::test]
async fn search_route_should_return_the_exact_upstream_error_response() {
    let response = api_router(SearchExecution::new(true))
        .await
        .oneshot(
            Request::post("/v1/alpha/search")
                .header(AUTHORIZATION, "Bearer sk_search_test")
                .header("content-type", "application/json")
                .body(Body::from(
                    br#"{ "model":"gpt-future", "future_invalid":9007199254740993 }"#.to_vec(),
                ))
                .expect("search request"),
        )
        .await
        .expect("search response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response.headers().get("content-type"),
        Some(&"application/problem+json".parse().expect("content type"))
    );
    assert_eq!(
        response.headers().get("x-future-search-error"),
        Some(&"preserved".parse().expect("header value"))
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read search response");
    assert_eq!(
        body.as_ref(),
        br#"{ "error":{"message":"future search validation"}, "future":9007199254740993 }"#
    );
}
