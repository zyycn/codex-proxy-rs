//! 按显式协议头投影模型目录，Codex 目录保留独立合同

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use gateway_core::{
    engine::execution::AuthenticatedClient,
    routing::{ProviderCatalogUnavailable, PublicModelDescriptor, PublicModelId},
};
use gateway_protocol::anthropic::ANTHROPIC_PROTOCOL;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ApiState,
    data_plane::middleware::{self, RequestInput},
};

use super::{
    auth::{ClientKeyHeaders, authenticate_client, client_access_error_response},
    error::{catalog_unavailable_response, model_not_found_response, openai_error_response},
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

#[derive(Debug, Deserialize)]
pub(crate) struct ModelsQuery {
    client_version: Option<String>,
    after_id: Option<String>,
    before_id: Option<String>,
    limit: Option<String>,
}

/// `GET /v1/models`
/// Codex 携带 `client_version` 时返回其专用目录合同
pub(crate) async fn models(
    State(state): State<ApiState>,
    Query(query): Query<ModelsQuery>,
    headers: HeaderMap,
) -> Response {
    let anthropic = headers.contains_key("anthropic-version") || headers.contains_key("x-api-key");
    let service = state.data_plane();
    let client = match authenticate_client(service, &headers, ClientKeyHeaders::Anthropic).await {
        Ok(client) => client,
        Err(error) => {
            return model_protocol_response(client_access_error_response(error), anthropic).await;
        }
    };

    let mut input = RequestInput::query(super::router::MODELS_PATH.to_owned(), None, &headers);
    if anthropic {
        input.protocol = ANTHROPIC_PROTOCOL.to_owned();
    }
    let response = middleware::query_response(service.execution(), client, input, move |client| {
        models_response(state, client, query, anthropic)
    })
    .await;
    model_protocol_response(response, anthropic).await
}

async fn models_response(
    state: ApiState,
    client: AuthenticatedClient,
    query: ModelsQuery,
    anthropic: bool,
) -> Response {
    let service = state.data_plane();

    if anthropic {
        return match messages_models(service, &client).await {
            Ok(models) => anthropic_models_response(&models, &query),
            Err(_) => catalog_unavailable_response(),
        };
    }
    if let Some(version) = query
        .client_version
        .as_deref()
        .filter(|version| !version.trim().is_empty())
    {
        return super::codex::model_catalog(service, &client, version).await;
    }

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

/// `GET /v1/models/{model_id}`
pub(crate) async fn model_detail(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> Response {
    let anthropic = headers.contains_key("anthropic-version") || headers.contains_key("x-api-key");
    let service = state.data_plane();
    let client = match authenticate_client(service, &headers, ClientKeyHeaders::Anthropic).await {
        Ok(client) => client,
        Err(error) => {
            return model_protocol_response(client_access_error_response(error), anthropic).await;
        }
    };
    let mut input = RequestInput::query(
        format!("{}/{model_id}", super::router::MODELS_PATH),
        Some(model_id.clone()),
        &headers,
    );
    if anthropic {
        input.protocol = ANTHROPIC_PROTOCOL.to_owned();
    }
    let response =
        middleware::query_response(
            service.execution(),
            client,
            input,
            move |client| async move {
                model_detail_response(&state, &client, model_id, anthropic).await
            },
        )
        .await;
    model_protocol_response(response, anthropic).await
}

async fn model_detail_response(
    state: &ApiState,
    client: &AuthenticatedClient,
    model_id: String,
    anthropic: bool,
) -> Response {
    let service = state.data_plane();
    let Ok(public_model) = PublicModelId::new(model_id) else {
        return model_not_found_response().into_response();
    };
    let visible = if anthropic {
        match messages_models(service, client).await {
            Ok(models) => models.binary_search(&public_model).is_ok(),
            Err(_) => return catalog_unavailable_response(),
        }
    } else {
        service.contains_public_model(client, &public_model)
    };
    if !visible {
        return model_not_found_response().into_response();
    }

    (
        StatusCode::OK,
        Json(if anthropic {
            anthropic_model_json(public_model.as_str())
        } else {
            openai_model_json(public_model.as_str())
        }),
    )
        .into_response()
}

async fn messages_models(
    service: &super::service::DataPlaneService,
    client: &AuthenticatedClient,
) -> Result<Vec<PublicModelId>, ProviderCatalogUnavailable> {
    let context = crate::middleware::current();
    let entries = service
        .execution()
        .client_model_catalog(
            client,
            ANTHROPIC_PROTOCOL,
            "",
            context.as_ref().and_then(|ctx| ctx.extensions.peer_route()),
        )
        .await?;
    let mut models: Vec<_> = entries
        .into_iter()
        .map(PublicModelDescriptor::into_model)
        .collect();
    models.sort_unstable();
    Ok(models)
}

async fn model_protocol_response(response: Response, anthropic: bool) -> Response {
    if anthropic {
        super::messages::project_error_response(response).await
    } else {
        response
    }
}

fn anthropic_models_response(models: &[PublicModelId], query: &ModelsQuery) -> Response {
    let limit = match query.limit.as_deref().unwrap_or("20").parse::<usize>() {
        Ok(limit @ 1..=1000) => limit,
        _ => {
            return openai_error_response(
                StatusCode::BAD_REQUEST,
                "Model list limit must be between 1 and 1000.",
                "invalid_request_error",
                "invalid_limit",
            )
            .into_response();
        }
    };
    let cursor = |value: &Option<String>| -> Result<Option<usize>, ()> {
        value
            .as_ref()
            .map(|id| {
                models
                    .iter()
                    .position(|model| model.as_str() == id)
                    .ok_or(())
            })
            .transpose()
    };
    let (Ok(after), Ok(before)) = (cursor(&query.after_id), cursor(&query.before_id)) else {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "The model pagination cursor was not found.",
            "invalid_request_error",
            "invalid_cursor",
        )
        .into_response();
    };
    let start = after.map_or(0, |index| index + 1);
    let end = before.unwrap_or(models.len());
    let range = if before.is_some() {
        start.max(end.saturating_sub(limit))..end
    } else {
        start..end.min(start.saturating_add(limit))
    };
    let data = models
        .get(range.clone())
        .unwrap_or_default()
        .iter()
        .map(|model| anthropic_model_json(model.as_str()))
        .collect::<Vec<_>>();
    let has_more = if before.is_some() {
        range.start > start
    } else {
        range.end < end
    };
    Json(json!({"first_id":data.first().and_then(|model| model.get("id")), "last_id":data.last().and_then(|model| model.get("id")), "has_more":has_more, "data":data})).into_response()
}

fn anthropic_model_json(id: &str) -> Value {
    json!({"id":id, "type":"model", "display_name":id, "created_at":"1970-01-01T00:00:00Z", "capabilities":null, "max_input_tokens":null, "max_tokens":null, "line":null, "lifecycle":"active", "deprecated_at":null, "retires_at":null})
}

fn openai_model_json(id: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": MODEL_CREATED_TIMESTAMP,
        "owned_by": "gateway",
    })
}
