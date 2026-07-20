use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::future::BoxFuture;
use gateway_core::{
    engine::execution::{
        AuthenticatedClient, ClientAuthenticationError, ExecutionService, StartExecution,
        StartedExecution,
    },
    error::GatewayError,
    routing::PublicModelId,
};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_should_return_no_content_when_all_inputs_are_healthy() {
    let response = crate::openai::api_router(Arc::new(UnusedExecution))
        .await
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("health request"),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

struct UnusedExecution;

impl ExecutionService for UnusedExecution {
    fn authenticate(&self, _: &str) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        unreachable!("health check does not authenticate")
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        unreachable!("health check does not list models")
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, _: &PublicModelId) -> bool {
        unreachable!("health check does not inspect models")
    }

    fn start(&self, _: StartExecution) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { unreachable!("health check does not execute requests") })
    }
}
