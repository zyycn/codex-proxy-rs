use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use futures::future::BoxFuture;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionService, StartExecution,
    StartedExecution,
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::routing::PublicModelId;
use tower::ServiceExt;

use super::{api_router, authenticated_client};

struct ModelsExecution {
    client: AuthenticatedClient,
}

impl ModelsExecution {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            client: authenticated_client("sk_models_test"),
        })
    }
}

impl ExecutionService for ModelsExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        if plaintext == "sk_models_test" {
            Ok(self.client.clone())
        } else {
            Err(ClientAuthenticationError::InvalidKey)
        }
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        ["model-a", "model-b"]
            .into_iter()
            .map(|model| PublicModelId::new(model).expect("model"))
            .collect()
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, model: &PublicModelId) -> bool {
        matches!(model.as_str(), "model-a" | "model-b")
    }

    fn start(&self, _: StartExecution) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "models test must not start a response",
            ))
        })
    }
}

fn authorized_request(path: &str) -> Request<Body> {
    Request::get(path)
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .body(Body::empty())
        .expect("build models request")
}

#[tokio::test]
async fn models_should_encode_the_service_visible_catalog() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(authorized_request("/v1/models"))
        .await
        .expect("list models response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read models body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("models JSON");

    assert_eq!(
        value,
        serde_json::json!({
            "object": "list",
            "data": [
                {"id":"model-a","object":"model","created":1700000000_i64,"owned_by":"gateway"},
                {"id":"model-b","object":"model","created":1700000000_i64,"owned_by":"gateway"}
            ]
        })
    );
}

#[tokio::test]
async fn model_detail_should_keep_the_official_path_id_contract() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(authorized_request("/v1/models/model-a"))
        .await
        .expect("model detail response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn model_detail_should_hide_unknown_models() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(authorized_request("/v1/models/model-private"))
        .await
        .expect("unknown model response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn models_should_require_a_valid_bearer_client_key() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(
            Request::get("/v1/models")
                .body(Body::empty())
                .expect("build unauthenticated request"),
        )
        .await
        .expect("unauthenticated models response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
