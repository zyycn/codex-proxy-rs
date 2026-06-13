use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

use crate::{codex::accounts::models::catalog::ModelCatalog, runtime::state::AppState};

use super::{
    auth::authorize_client_api_key,
    errors::{missing_client_api_key_response, model_not_found_response},
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

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

pub async fn model_catalog(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = model_catalog_for_state(&state).await;
    (StatusCode::OK, Json(json!(catalog.models())))
}

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

pub async fn debug_models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
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
