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
use gateway_core::routing::{ModelPresentation, PublicModelId, PublicModelProfile};
use tower::ServiceExt;

use super::{api_router, authenticated_client};

struct ModelsExecution {
    client: AuthenticatedClient,
    profiles: bool,
}

impl ModelsExecution {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            client: authenticated_client("sk_models_test"),
            profiles: false,
        })
    }

    fn with_profiles() -> Arc<Self> {
        Arc::new(Self {
            client: authenticated_client("sk_models_test"),
            profiles: true,
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

    fn public_model_profiles(&self, _: &AuthenticatedClient) -> Vec<PublicModelProfile> {
        if !self.profiles {
            return Vec::new();
        }
        vec![PublicModelProfile::new(
            PublicModelId::new("grok-4.5").expect("model"),
            ModelPresentation::new(
                Some("Grok 4.5".to_owned()),
                Some("xAI Grok 4.5 frontier model.".to_owned()),
            )
            .with_reasoning(
                Some("medium".to_owned()),
                ["low", "medium", "high", "xhigh"]
                    .map(str::to_owned)
                    .to_vec(),
            )
            .with_context_window_tokens(Some(500_000))
            .with_image_input(true)
            .with_agent_tools(true, true),
        )]
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
async fn models_should_encode_provider_profiles_for_current_codex_clients() {
    let response = api_router(ModelsExecution::with_profiles())
        .await
        .oneshot(authorized_request("/v1/models?client_version=0.145.0"))
        .await
        .expect("list Codex models response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read Codex models body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("models JSON");
    let model = &value["models"][0];

    assert_eq!(model["slug"], "grok-4.5");
    assert_eq!(model["default_reasoning_level"], "medium");
    assert_eq!(model["context_window"], 500_000);
    assert_eq!(model["apply_patch_tool_type"], "freeform");
    assert_eq!(model["service_tiers"], serde_json::json!([]));
    assert_eq!(
        model["supported_reasoning_levels"],
        serde_json::json!([
            {
                "effort": "low",
                "description": "Fast responses with lighter reasoning"
            },
            {
                "effort": "medium",
                "description": "Balances speed and reasoning depth for everyday tasks"
            },
            {
                "effort": "high",
                "description": "Greater reasoning depth for complex problems"
            },
            {
                "effort": "xhigh",
                "description": "Extra high reasoning depth for complex problems"
            }
        ])
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
async fn model_catalog_should_keep_the_codex_catalog_contract() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(authorized_request("/v1/models/catalog"))
        .await
        .expect("catalog response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read catalog body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("catalog JSON");
    assert_eq!(value[0]["id"], "model-a");
    assert_eq!(value[0]["displayName"], "model-a");
    assert_eq!(value[0]["source"], "gateway");
}

#[tokio::test]
async fn model_info_should_return_a_single_visible_catalog_entry() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(authorized_request("/v1/models/model-a/info"))
        .await
        .expect("model info response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn model_info_should_hide_unknown_models() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(authorized_request("/v1/models/model-private/info"))
        .await
        .expect("missing model info response");

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
