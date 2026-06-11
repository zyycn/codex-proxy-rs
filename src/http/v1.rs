use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    http::auth::client_api_key, models::catalog::ModelCatalog, state::AppState,
    translation::codex_to_openai::openai_error,
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

#[derive(Deserialize)]
struct ResponsesBody {
    model: Option<String>,
}

pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !has_client_api_key(&headers) {
        return missing_client_api_key_response();
    }

    let default_model = state.config().model.default_model.clone();
    let body = serde_json::from_slice::<ResponsesBody>(&body).unwrap_or_else(|_| ResponsesBody {
        model: Some(default_model.clone()),
    });
    let model = body.model.unwrap_or(default_model);
    let catalog = ModelCatalog::from_config(&state.config().model);
    if !catalog.is_recognized_model_name(&model) {
        return (
            StatusCode::NOT_FOUND,
            Json(openai_error("Model not found", "model_not_found")),
        );
    }

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(openai_error(
            "No available Codex accounts",
            "no_available_accounts",
        )),
    )
}

pub async fn models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !has_client_api_key(&headers) {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
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
    if !has_client_api_key(&headers) {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    (StatusCode::OK, Json(json!(catalog.models())))
}

pub async fn model_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    if !has_client_api_key(&headers) {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
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
    if !has_client_api_key(&headers) {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    let Some(info) = catalog.model_info(&model_id) else {
        return model_not_found_response();
    };
    (StatusCode::OK, Json(json!(info)))
}

pub async fn debug_models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !has_client_api_key(&headers) {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    (StatusCode::OK, Json(json!(catalog.debug())))
}

fn has_client_api_key(headers: &HeaderMap) -> bool {
    client_api_key(headers).is_some()
}

fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(openai_error("Missing client API key", "invalid_api_key")),
    )
}

fn model_not_found_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(openai_error("Model not found", "model_not_found")),
    )
}

fn openai_model_json(id: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": MODEL_CREATED_TIMESTAMP,
        "owned_by": "openai"
    })
}
