//! 按显式协议头投影模型目录，Codex 目录保留独立合同

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use gateway_core::{
    engine::execution::AuthenticatedClient,
    routing::{PublicModelDescriptor, PublicModelId, PublicModelProfile},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ApiState,
    openai::middleware::{self, RequestInput},
};

use super::{
    auth::{ClientKeyHeaders, authenticate_client, client_access_error_response},
    error::{model_not_found_response, openai_error_response},
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;
const CODEX_BASE_INSTRUCTIONS: &str = "You are Codex, a coding agent. Follow the user's instructions and use the available tools to complete software engineering tasks. Inspect relevant files before editing, preserve unrelated changes, and verify the result.";

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
    let service = state.openai();
    let client = match authenticate_client(service, &headers, ClientKeyHeaders::Anthropic).await {
        Ok(client) => client,
        Err(error) => {
            return model_protocol_response(client_access_error_response(error), anthropic).await;
        }
    };

    let mut input = RequestInput::query(super::router::MODELS_PATH.to_owned(), None, &headers);
    if anthropic {
        input.protocol = "anthropic".to_owned();
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
    let service = state.openai();

    if anthropic {
        return anthropic_models_response(service.public_models(&client), &query);
    }
    if let Some(version) = query
        .client_version
        .as_deref()
        .filter(|version| !version.trim().is_empty())
    {
        if version.len() > 64
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
        {
            return openai_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid client_version.",
                "invalid_request_error",
                "invalid_client_version",
            )
            .into_response();
        }
        let catalog = match service.client_model_catalog(&client, version).await {
            Ok(catalog) => catalog,
            Err(_) => return catalog_unavailable_response(),
        };
        let models = catalog
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                PublicModelDescriptor::Adapted(profile) => {
                    Ok(adapted_codex_model_json(profile, index))
                }
                PublicModelDescriptor::Native { model, payload } => {
                    if payload.protocol() != "codex" {
                        return Err(());
                    }
                    let mut value: Value =
                        serde_json::from_slice(payload.body()).map_err(|_| ())?;
                    let object = value.as_object_mut().ok_or(())?;
                    // 别名只改变请求标识，能力、提示词、顺序等仍由上游目录决定
                    object.insert("slug".to_owned(), Value::String(model.as_str().to_owned()));
                    Ok(value)
                }
            })
            .collect::<Result<Vec<_>, ()>>();
        let Ok(models) = models else {
            return catalog_unavailable_response();
        };
        return (
            StatusCode::OK,
            // 目录经过账号选择/别名/多 Provider 聚合，不能借用上游 ETag 表示改写后的正文
            [("cache-control", "private, no-store")],
            Json(json!({
                "models": models,
            })),
        )
            .into_response();
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

fn catalog_unavailable_response() -> Response {
    openai_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "Model catalog is temporarily unavailable.",
        "server_error",
        "model_catalog_unavailable",
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
    let service = state.openai();
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
        input.protocol = "anthropic".to_owned();
    }
    let response = middleware::query_response(
        service.execution(),
        client,
        input,
        move |client| async move { model_detail_response(&state, &client, model_id, anthropic) },
    )
    .await;
    model_protocol_response(response, anthropic).await
}

fn model_detail_response(
    state: &ApiState,
    client: &AuthenticatedClient,
    model_id: String,
    anthropic: bool,
) -> Response {
    let service = state.openai();
    let Ok(public_model) = PublicModelId::new(model_id) else {
        return model_not_found_response().into_response();
    };
    if !service.contains_public_model(client, &public_model) {
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

async fn model_protocol_response(response: Response, anthropic: bool) -> Response {
    if anthropic {
        super::messages::project_error_response(response).await
    } else {
        response
    }
}

fn anthropic_models_response(models: Vec<String>, query: &ModelsQuery) -> Response {
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
            .map(|id| models.iter().position(|model| model == id).ok_or(()))
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
        .map(|model| anthropic_model_json(model))
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

fn adapted_codex_model_json(profile: &PublicModelProfile, index: usize) -> Value {
    let id = profile.model().as_str();
    let presentation = profile.presentation();
    let reasoning_levels = presentation
        .supported_reasoning_efforts()
        .iter()
        .map(|effort| {
            json!({
                "effort": effort,
                "description": reasoning_effort_description(effort),
            })
        })
        .collect::<Vec<_>>();
    let reasoning_summary = presentation.reasoning_summary();
    let context_window = presentation.context_window_tokens();
    let max_context_window = presentation.max_context_window_tokens();
    let input_modalities = if presentation.image_input() {
        vec!["text", "image"]
    } else {
        vec!["text"]
    };
    let additional_speed_tiers = presentation
        .service_tiers()
        .iter()
        .filter_map(|tier| tier.speed_tier())
        .collect::<Vec<_>>();
    let service_tiers = presentation
        .service_tiers()
        .iter()
        .map(|tier| {
            json!({
                "id": tier.id(),
                "name": tier.name(),
                "description": tier.description(),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "slug": id,
        "display_name": presentation.display_name().unwrap_or(id),
        "description": presentation.description(),
        "default_reasoning_level": presentation.default_reasoning_effort(),
        "supported_reasoning_levels": reasoning_levels,
        "shell_type": "shell_command",
        "visibility": if presentation.hidden() { "hide" } else { "list" },
        "supported_in_api": true,
        "priority": index.saturating_add(1),
        "additional_speed_tiers": additional_speed_tiers,
        "service_tiers": service_tiers,
        "availability_nux": Value::Null,
        "upgrade": Value::Null,
        "base_instructions": CODEX_BASE_INSTRUCTIONS,
        "model_messages": Value::Null,
        "include_skills_usage_instructions": false,
        "supports_reasoning_summary_parameter": reasoning_summary,
        "default_reasoning_summary": if reasoning_summary { "auto" } else { "none" },
        "support_verbosity": presentation.verbosity(),
        "default_verbosity": Value::Null,
        "apply_patch_tool_type": presentation.agent_tools().then_some("freeform"),
        "web_search_tool_type": "text",
        "truncation_policy": { "mode": "tokens", "limit": 10_000 },
        "supports_parallel_tool_calls": presentation.parallel_tool_calls(),
        "supports_image_detail_original": presentation.image_detail_original(),
        "context_window": context_window,
        "max_context_window": max_context_window,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": input_modalities,
        "supports_search_tool": presentation.search_tool(),
        // 通用画像没有 Responses Lite 支持依据，避免客户端启用未经声明的协议行为
        // 原生目录由 Native 分支保留上游字段，不经过此处
        "use_responses_lite": false,
    })
}

fn reasoning_effort_description(effort: &str) -> &'static str {
    match effort {
        "none" => "No reasoning",
        "minimal" => "Minimal reasoning",
        "low" => "Fast responses with lighter reasoning",
        "medium" => "Balances speed and reasoning depth for everyday tasks",
        "high" => "Greater reasoning depth for complex problems",
        "xhigh" => "Extra high reasoning depth for complex problems",
        "max" => "Maximum reasoning depth for the hardest problems",
        _ => "Provider-supported reasoning level",
    }
}
