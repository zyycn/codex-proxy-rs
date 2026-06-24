//! OpenAI 模型列表以及模型处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{proxy::auth::authorize_client_api_key, runtime::state::AppState};

use super::errors::{missing_client_api_key_response, model_not_found_response};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

// ====================================================================
// OpenAI 模型类型
// ====================================================================

/// OpenAI 模型对象。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiModel {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
}

/// OpenAI 模型列表。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiModelList {
    pub object: String,
    pub data: Vec<OpenAiModel>,
}

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

pub async fn model_catalog_for_state(state: &AppState) -> crate::upstream::models::ModelCatalog {
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
