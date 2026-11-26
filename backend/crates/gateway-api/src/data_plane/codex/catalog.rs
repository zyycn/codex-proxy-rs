//! Codex 模型目录的原生对象保留、通用画像投影与客户端权限裁剪

use crate::data_plane::{error::openai_error_response, service::DataPlaneService};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use gateway_core::{
    engine::execution::AuthenticatedClient,
    routing::{PublicModelDescriptor, PublicModelProfile},
};
use serde_json::{Value, json};

const CODEX_BASE_INSTRUCTIONS: &str = "You are Codex, a coding agent. Follow the user's instructions and use the available tools to complete software engineering tasks. Inspect relevant files before editing, preserve unrelated changes, and verify the result.";

pub(crate) async fn model_catalog(
    service: &DataPlaneService,
    client: &AuthenticatedClient,
    version: &str,
) -> Response {
    let context = crate::middleware::current();
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
    let catalog = match service
        .execution()
        .client_model_catalog(
            client,
            "codex",
            version,
            context.as_ref().and_then(|ctx| ctx.extensions.peer_route()),
        )
        .await
    {
        Ok(catalog) => catalog,
        Err(_) => return catalog_unavailable_response(),
    };
    let models = catalog
        .iter()
        .enumerate()
        .map(|(index, entry)| match entry {
            PublicModelDescriptor::Adapted(profile) => Ok(adapted_codex_model_json(profile, index)),
            PublicModelDescriptor::Native { model, payload } => {
                if payload.protocol() != "codex" {
                    return Err(());
                }
                let mut value: Value = serde_json::from_slice(payload.body()).map_err(|_| ())?;
                let object = value.as_object_mut().ok_or(())?;
                // 别名只改变请求标识，能力、提示词、顺序等仍由上游目录决定
                object.insert("slug".to_owned(), Value::String(model.as_str().to_owned()));
                Ok(value)
            }
        })
        .collect::<Result<Vec<_>, ()>>();
    let Ok(mut models) = models else {
        return catalog_unavailable_response();
    };
    if !client.policy().account_scope().allows_subagents() {
        for model in &mut models {
            model["multi_agent_version"] = json!("disabled");
        }
    }
    (
        StatusCode::OK,
        // 目录经过账号选择/别名/多 Provider 聚合，不能借用上游 ETag 表示改写后的正文
        [("cache-control", "private, no-store")],
        Json(json!({
            "models": models,
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
