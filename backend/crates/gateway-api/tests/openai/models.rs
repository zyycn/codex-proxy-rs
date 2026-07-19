use std::collections::BTreeSet;

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use gateway_api::openai::{
    ConnectionTask, DeliveryEvent, OpenAiApiState, OpenAiClientService, ResponseExecutionSession,
    ResponsesTransport, StartedResponse, auth::ClientApiKeyAuthError,
    responses::DecodedResponsesRequest,
};
use gateway_core::{
    engine::EngineError,
    error::{GatewayError, GatewayErrorKind},
    event::GatewayEvent,
    routing::PublicModelId,
};
use tower::ServiceExt;

#[derive(Clone)]
struct ModelsState;

#[derive(Clone)]
struct ModelsService;

struct UnusedSession;

#[async_trait]
impl ResponseExecutionSession for UnusedSession {
    async fn next_delivery_event(&mut self) -> Result<Option<DeliveryEvent>, EngineError> {
        unreachable!("models endpoints do not start an execution")
    }

    async fn collect_uncommitted(&mut self) -> Result<Vec<GatewayEvent>, EngineError> {
        unreachable!("models endpoints do not start an execution")
    }

    async fn commit_downstream(
        &mut self,
        _client_status_code: Option<u16>,
    ) -> Result<(), EngineError> {
        unreachable!("models endpoints do not start an execution")
    }

    async fn record_client_status(&mut self, _client_status_code: u16) -> Result<(), EngineError> {
        unreachable!("models endpoints do not start an execution")
    }

    fn is_finalized(&self) -> bool {
        true
    }

    fn cancel(&self) {}

    fn detach_finalize(self) {}
}

#[async_trait]
impl OpenAiClientService for ModelsService {
    type Client = ();
    type Session = UnusedSession;

    fn authenticate(&self, plaintext: &str) -> Result<Self::Client, ClientApiKeyAuthError> {
        if plaintext == "sk_models_test" {
            Ok(())
        } else {
            Err(ClientApiKeyAuthError::InvalidKey)
        }
    }

    fn public_models(&self, _client: &Self::Client) -> Vec<String> {
        vec!["model-a".to_owned(), "model-b".to_owned()]
    }

    fn contains_public_model(&self, _client: &Self::Client, model: &PublicModelId) -> bool {
        BTreeSet::from(["model-a", "model-b"]).contains(model.as_str())
    }

    async fn start_response(
        &self,
        _client: Self::Client,
        _request: DecodedResponsesRequest,
        _transport: ResponsesTransport,
    ) -> Result<StartedResponse<Self::Session>, GatewayError> {
        Err(GatewayError::new(
            GatewayErrorKind::Internal,
            "models test must not start a response",
        ))
    }

    fn is_shutting_down(&self) -> bool {
        false
    }

    fn spawn_connection(&self, task: ConnectionTask) {
        drop(task);
    }

    fn next_connection_id(&self) -> String {
        "ws_models_test".to_owned()
    }

    fn next_request_id(&self) -> String {
        "req_models_test".to_owned()
    }
}

impl OpenAiApiState for ModelsState {
    type Service = ModelsService;

    fn openai_client_api(&self) -> Self::Service {
        ModelsService
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
    let response = gateway_api::openai::router::<ModelsState>()
        .with_state(ModelsState)
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
    let response = gateway_api::openai::router::<ModelsState>()
        .with_state(ModelsState)
        .oneshot(authorized_request("/v1/models/model-a"))
        .await
        .expect("model detail response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn model_detail_should_hide_unknown_models() {
    let response = gateway_api::openai::router::<ModelsState>()
        .with_state(ModelsState)
        .oneshot(authorized_request("/v1/models/model-private"))
        .await
        .expect("unknown model response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn models_should_require_a_valid_bearer_client_key() {
    let response = gateway_api::openai::router::<ModelsState>()
        .with_state(ModelsState)
        .oneshot(
            Request::get("/v1/models")
                .body(Body::empty())
                .expect("build unauthenticated request"),
        )
        .await
        .expect("unauthenticated models response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
