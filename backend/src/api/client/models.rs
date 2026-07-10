//! OpenAI 模型列表以及模型处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

use crate::{
    api::client::auth::authorize_client_api_key, api::AppState, models::catalog::ModelCatalog,
};

use super::errors::{missing_client_api_key_response, model_not_found_response};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

// ====================================================================
// HTTP 处理器
// ====================================================================

/// `GET /v1/models`
pub async fn models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = model_catalog_for_state(&state).await;
    let data = catalog
        .public_model_ids()
        .iter()
        .map(|model_id| openai_model_json(model_id))
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
    if !catalog.is_recognized_model_name(&model_id) {
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
    let Some(info) = catalog.model_info_for_name(&model_id) else {
        return model_not_found_response();
    };
    (StatusCode::OK, Json(json!(info)))
}

pub async fn model_catalog_for_state(state: &AppState) -> ModelCatalog {
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
