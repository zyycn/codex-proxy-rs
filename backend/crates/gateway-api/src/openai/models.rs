//! OpenAI 模型目录 HTTP adapter。

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use gateway_core::routing::PublicModelId;
use serde_json::{Value, json};

use crate::ApiState;

use super::{
    auth::{authenticate_client, authentication_error_response},
    error::model_not_found_response,
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

/// `GET /v1/models`。
pub(crate) async fn models(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let data = service
        .public_models(&client)
        .into_iter()
        .map(|model| openai_model_json(&model))
        .collect::<Vec<_>>();

    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": data,
        })),
    )
        .into_response()
}

/// `GET /v1/models/{model_id}`。
pub(crate) async fn model_detail(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let Ok(public_model) = PublicModelId::new(model_id) else {
        return model_not_found_response().into_response();
    };
    if !service.contains_public_model(&client, &public_model) {
        return model_not_found_response().into_response();
    }

    (
        StatusCode::OK,
        Json(openai_model_json(public_model.as_str())),
    )
        .into_response()
}

fn openai_model_json(id: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": MODEL_CREATED_TIMESTAMP,
        "owned_by": "gateway",
    })
}
