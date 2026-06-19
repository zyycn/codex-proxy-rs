//! Responses 创建编排。

use serde_json::{Map, Value};

use crate::{
    models::model::{ModelConfig, ParsedModelName},
    protocol::codex::responses::CodexResponsesRequest,
};

/// Codex Responses 请求的上游传输决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransport {
    /// 使用 HTTP SSE。
    HttpSse,
    /// 优先使用 WebSocket，失败时允许回退 HTTP SSE。
    WebSocketPreferred,
    /// 必须使用 WebSocket，不允许 HTTP SSE 回退。
    WebSocketRequired,
}

/// 判断 Responses 请求应使用哪种上游传输。
pub fn transport_for_request(request: &CodexResponsesRequest) -> CodexTransport {
    if request.force_http_sse {
        return CodexTransport::HttpSse;
    }

    if request.previous_response_id.is_some() {
        return CodexTransport::WebSocketRequired;
    }

    CodexTransport::WebSocketPreferred
}

/// 判断请求在 WebSocket 失败后是否允许 HTTP SSE 回退。
pub fn http_sse_fallback_allowed(request: &CodexResponsesRequest) -> bool {
    !matches!(
        transport_for_request(request),
        CodexTransport::WebSocketRequired
    )
}

/// 应用模型名后缀和模型默认值对应的 Responses 上游请求选项。
pub fn apply_response_model_options(
    request: &mut CodexResponsesRequest,
    parsed_model: &ParsedModelName,
    config: &ModelConfig,
) {
    request.model = parsed_model.model_id.clone();
    apply_reasoning_options(request, parsed_model, config);
    apply_service_tier_options(request, parsed_model, config);
}

fn apply_reasoning_options(
    request: &mut CodexResponsesRequest,
    parsed_model: &ParsedModelName,
    config: &ModelConfig,
) {
    let existing_reasoning = request.reasoning.take();
    let existing_object = match existing_reasoning {
        Some(Value::Object(object)) => object,
        Some(_) | None => Map::new(),
    };
    let effort = existing_object
        .get("effort")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| non_empty_string(parsed_model.reasoning_effort.as_deref()))
        .or_else(|| non_empty_string(config.default_reasoning_effort.as_deref()));
    if effort.is_none() && existing_object.is_empty() {
        request.reasoning = None;
        return;
    }

    let summary = existing_object
        .get("summary")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("auto");
    let mut reasoning = Map::new();
    reasoning.insert("summary".to_string(), Value::String(summary.to_string()));
    if let Some(effort) = effort {
        reasoning.insert("effort".to_string(), Value::String(effort));
    }
    request.reasoning = Some(Value::Object(reasoning));
    ensure_reasoning_include(request);
}

fn apply_service_tier_options(
    request: &mut CodexResponsesRequest,
    parsed_model: &ParsedModelName,
    config: &ModelConfig,
) {
    request.service_tier = request
        .service_tier
        .take()
        .and_then(|value| non_empty_string(Some(&value)))
        .or_else(|| non_empty_string(parsed_model.service_tier.as_deref()))
        .or_else(|| non_empty_string(config.service_tier.as_deref()))
        .map(normalize_service_tier_for_upstream);
}

fn ensure_reasoning_include(request: &mut CodexResponsesRequest) {
    if request.reasoning.is_none() {
        return;
    }
    if request
        .include
        .as_ref()
        .is_some_and(|include| !include.is_empty())
    {
        return;
    }
    request.include = Some(vec!["reasoning.encrypted_content".to_string()]);
}

fn normalize_service_tier_for_upstream(service_tier: String) -> String {
    if service_tier == "fast" {
        "priority".to_string()
    } else {
        service_tier
    }
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}
