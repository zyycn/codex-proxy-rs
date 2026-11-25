//! 独立搜索 HTTP 接口的请求传递、鉴权与响应交付测试

mod http;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
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
use gateway_core::operation::{Operation, OperationKind};
use gateway_core::routing::PublicModelId;
use gateway_core::upstream::UpstreamSendState;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::openai_endpoints::BufferedJsonSession;
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
    middleware: Option<Arc<crate::data_plane::middleware::RequestMiddleware>>,
}

impl SearchExecution {
    fn new(fail_with_upstream_response: bool) -> Arc<Self> {
        Arc::new(Self {
            client: authenticated_client("sk_search_test"),
            captured: Mutex::new(Vec::new()),
            committed_statuses: Arc::new(Mutex::new(Vec::new())),
            fail_with_upstream_response,
            middleware: None,
        })
    }

    fn captured(&self) -> Vec<CapturedSearchRequest> {
        self.captured.lock().expect("capture lock").clone()
    }
}

impl ExecutionService for SearchExecution {
    fn middleware_plan(
        &self,
        _: &gateway_core::engine::execution::PreparedRootExecution,
    ) -> Option<gateway_core::engine::middleware::FrozenMiddlewarePlan> {
        self.middleware.as_ref().map(|plan| plan.frozen())
    }

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
