//! OpenAI 模型处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

use codex_proxy_core::models::catalog::ModelCatalog;
use codex_proxy_runtime::state::AppState;

use super::{auth::authorize_client_api_key, diagnostics::is_local_debug_request};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

/// `GET /v1/models`
pub async fn models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = model_catalog_for_state(&state).await;
    let data = catalog
        .models()
        .iter()
        .map(|model| openai_model_json(&model.id))
        .collect::<Vec<_>>();
    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": data
        })),
    )
}

/// `GET /v1/models/catalog`
pub async fn model_catalog(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = model_catalog_for_state(&state).await;
    (StatusCode::OK, Json(json!(catalog.models())))
}

/// `GET /v1/models/{model_id}`
pub async fn model_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = model_catalog_for_state(&state).await;
    if catalog.model_info(&model_id).is_none() {
        return model_not_found_response();
    }
    (StatusCode::OK, Json(openai_model_json(&model_id)))
}

/// `GET /v1/models/{model_id}/info`
pub async fn model_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = model_catalog_for_state(&state).await;
    let Some(info) = catalog.model_info(&model_id) else {
        return model_not_found_response();
    };
    (StatusCode::OK, Json(json!(info)))
}

/// `GET /debug/models`
pub async fn debug_models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "debug endpoint is local-only" })),
        );
    }

    let catalog = model_catalog_for_state(&state).await;
    (StatusCode::OK, Json(json!(catalog.debug())))
}

pub(super) async fn model_catalog_for_state(state: &AppState) -> ModelCatalog {
    state.services.models.catalog().await
}

fn openai_model_json(id: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": MODEL_CREATED_TIMESTAMP,
        "owned_by": "openai"
    })
}

fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": "Missing client API key",
                "type": "invalid_request_error",
                "code": "invalid_api_key"
            }
        })),
    )
}

fn model_not_found_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "message": "Model not found",
                "type": "invalid_request_error",
                "code": "model_not_found"
            }
        })),
    )
}
