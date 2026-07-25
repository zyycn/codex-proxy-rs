//! OpenAI 模型目录 HTTP adapter。

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use gateway_core::routing::{PublicModelId, PublicModelProfile};
use serde_json::{Value, json};

use crate::ApiState;

use super::{
    auth::{authenticate_client, authentication_error_response},
    error::model_not_found_response,
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;
const CODEX_BASE_INSTRUCTIONS: &str = "You are Codex, a coding agent. Follow the user's instructions and use the available tools to complete software engineering tasks. Inspect relevant files before editing, preserve unrelated changes, and verify the result.";

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
    let profiles = service.public_model_profiles(&client);

    if !profiles.is_empty() {
        let models = profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| codex_model_json(profile, index))
            .collect::<Vec<_>>();
        return (
            StatusCode::OK,
            Json(json!({
                "object": "list",
                "data": data,
                "models": models,
            })),
        )
            .into_response();
    }

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

/// `GET /v1/models/catalog`。
///
/// 新架构的目录由 core 按客户端 Provider 范围裁剪；这里不读取或重建任一
/// Provider 的 wire catalog，只恢复 Codex 客户端依赖的稳定展示合同。
pub(crate) async fn model_catalog(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let models = service
        .public_models(&client)
        .into_iter()
        .map(|model| catalog_model_json(model.as_str()))
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(models)).into_response()
}

/// `GET /v1/models/{model_id}/info`。
pub(crate) async fn model_info(
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
        Json(catalog_model_json(public_model.as_str())),
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

fn catalog_model_json(id: &str) -> Value {
    json!({
        "id": id,
        "displayName": id,
        "description": "",
        "isDefault": false,
        "supportedReasoningEfforts": [],
        "defaultReasoningEffort": "",
        "inputModalities": ["text"],
        "outputModalities": ["text"],
        "supportsPersonality": false,
        "upgrade": Value::Null,
        "source": "gateway",
    })
}

fn codex_model_json(profile: &PublicModelProfile, index: usize) -> Value {
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
    let reasoning_supported = presentation
        .supported_reasoning_efforts()
        .iter()
        .any(|effort| effort != "none");
    let context_window = presentation.context_window_tokens();
    let input_modalities = if presentation.image_input() {
        vec!["text", "image"]
    } else {
        vec!["text"]
    };

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
        "additional_speed_tiers": [],
        "service_tiers": [],
        "availability_nux": Value::Null,
        "upgrade": Value::Null,
        "base_instructions": CODEX_BASE_INSTRUCTIONS,
        "model_messages": Value::Null,
        "include_skills_usage_instructions": false,
        "supports_reasoning_summary_parameter": reasoning_supported,
        "default_reasoning_summary": "auto",
        "support_verbosity": presentation.verbosity(),
        "default_verbosity": Value::Null,
        "apply_patch_tool_type": presentation.agent_tools().then_some("freeform"),
        "web_search_tool_type": "text",
        "truncation_policy": { "mode": "tokens", "limit": 10_000 },
        "supports_parallel_tool_calls": presentation.parallel_tool_calls(),
        "supports_image_detail_original": presentation.image_detail_original(),
        "context_window": context_window,
        "max_context_window": context_window,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": input_modalities,
        "supports_search_tool": presentation.search_tool(),
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
