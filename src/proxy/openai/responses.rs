//! OpenAI Responses API 类型、协议转换与 HTTP 处理器。

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::net::IpAddr;

use crate::{
    http::middleware::request_id::RequestId,
    proxy::{
        auth::authorize_client_api_key,
        dispatch::responses::{ResponseDispatchError, ResponseDispatchStream},
    },
    runtime::state::AppState,
    upstream::protocol::{
        responses::{ensure_reasoning_include, CodexCompactRequest, CodexResponsesRequest},
        schema::prepare_schema,
        sse::{encode_sse_event, sse_body_has_done},
    },
};

use super::{
    errors::{
        invalid_responses_request_response, missing_client_api_key_response,
        model_not_found_response, responses_compact_dispatch_error_response,
        responses_dispatch_error_response, responses_stream_dispatch_failed_sse_event,
    },
    models::model_catalog_for_state,
    sse::{done_sse_frame, event_stream_response as sse_event_stream_response, SseResponseOptions},
};

const OPENAI_SUBAGENT_HEADER: &str = "x-openai-subagent";

// ====================================================================
// 协议类型
// ====================================================================

/// Responses API 请求体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenAiResponsesRequest {
    pub model: String,
    #[serde(default = "default_responses_input")]
    pub input: Value,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub reasoning: Option<Value>,
    #[serde(default)]
    pub tools: Option<Value>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub text: Option<Value>,
    #[serde(default)]
    pub generate: Option<bool>,
    #[serde(default)]
    pub prompt_cache_key: Option<String>,
    #[serde(default)]
    pub include: Option<Value>,
    #[serde(default)]
    pub client_metadata: Option<Value>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    #[serde(default, rename = "turnState", alias = "turn_state")]
    pub turn_state: Option<String>,
    #[serde(default, rename = "turnMetadata", alias = "turn_metadata")]
    pub turn_metadata: Option<String>,
    #[serde(default, rename = "betaFeatures", alias = "beta_features")]
    pub beta_features: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(
        default,
        rename = "includeTimingMetrics",
        alias = "include_timing_metrics"
    )]
    pub include_timing_metrics: Option<String>,
    #[serde(default, rename = "codexWindowId", alias = "codex_window_id")]
    pub codex_window_id: Option<String>,
    #[serde(default, rename = "parentThreadId", alias = "parent_thread_id")]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub use_websocket: Option<bool>,
    #[serde(default = "default_responses_stream")]
    pub stream: bool,
}

fn default_responses_input() -> Value {
    Value::Array(Vec::new())
}

fn default_responses_stream() -> bool {
    true
}

// ====================================================================
// 协议转换
// ====================================================================

/// 将 OpenAI Responses 请求转换为 Codex Responses 请求。
pub fn translate_response_to_codex(request: OpenAiResponsesRequest) -> CodexResponsesRequest {
    let prepared_text = prepare_text_format(request.text, true);
    let client_metadata = sanitize_client_metadata(request.client_metadata);
    let mut codex_request = CodexResponsesRequest::new_http_sse(
        request.model,
        request.instructions.unwrap_or_default(),
        sanitize_responses_input(request.input),
    );
    codex_request.previous_response_id = request.previous_response_id;
    codex_request.turn_state = request.turn_state;
    codex_request.turn_metadata = first_request_string(
        request.turn_metadata,
        client_metadata.as_ref(),
        "x-codex-turn-metadata",
    );
    codex_request.beta_features = first_request_string(
        request.beta_features,
        client_metadata.as_ref(),
        "x-codex-beta-features",
    );
    codex_request.include_timing_metrics = first_request_string(
        request.include_timing_metrics,
        client_metadata.as_ref(),
        "x-responsesapi-include-timing-metrics",
    );
    codex_request.version = non_empty_string(request.version);
    codex_request.codex_window_id = first_request_string(
        request.codex_window_id,
        client_metadata.as_ref(),
        "x-codex-window-id",
    );
    codex_request.parent_thread_id = first_request_string(
        request.parent_thread_id,
        client_metadata.as_ref(),
        "x-codex-parent-thread-id",
    );
    codex_request.reasoning = responses_reasoning(request.reasoning);
    codex_request.tools = non_empty_array(request.tools);
    codex_request.tool_choice = request.tool_choice;
    codex_request.parallel_tool_calls = request.parallel_tool_calls;
    codex_request.text = prepared_text.text;
    codex_request.tuple_schema = prepared_text.tuple_schema;
    codex_request.generate = request.generate;
    codex_request.service_tier = non_empty_string(request.service_tier);
    codex_request.prompt_cache_key = non_empty_string(request.prompt_cache_key);
    codex_request.explicit_prompt_cache_key = codex_request.prompt_cache_key.is_some();
    codex_request.include = string_array(request.include);
    codex_request.client_metadata = client_metadata;
    ensure_reasoning_include(&mut codex_request);
    match request.use_websocket {
        Some(true) => codex_request.use_websocket = true,
        Some(false) => codex_request.force_http_sse = true,
        None => {}
    }
    codex_request
}

/// 将 OpenAI Responses 请求转换为 Codex compact 请求。
pub fn translate_response_to_compact(request: OpenAiResponsesRequest) -> CodexCompactRequest {
    CodexCompactRequest {
        model: request.model,
        input: sanitize_responses_input(request.input),
        instructions: request.instructions.unwrap_or_default(),
        tools: non_empty_array(request.tools),
        parallel_tool_calls: request.parallel_tool_calls,
        reasoning: compact_reasoning(request.reasoning),
        text: prepare_text_format(request.text, false).text,
        client_ip: None,
        client_user_agent: None,
    }
}

struct PreparedTextFormat {
    text: Option<Value>,
    tuple_schema: Option<Value>,
}

fn prepare_text_format(text: Option<Value>, prepare_tuple_schema: bool) -> PreparedTextFormat {
    let Some(Value::Object(text)) = text else {
        return PreparedTextFormat {
            text: None,
            tuple_schema: None,
        };
    };
    let Some(Value::Object(format)) = text.get("format") else {
        return PreparedTextFormat {
            text: None,
            tuple_schema: None,
        };
    };
    let Some(format_type) = format.get("type").and_then(Value::as_str) else {
        return PreparedTextFormat {
            text: None,
            tuple_schema: None,
        };
    };

    let mut sanitized_format = Map::new();
    sanitized_format.insert("type".to_string(), Value::String(format_type.to_string()));
    if let Some(name) = format.get("name").and_then(Value::as_str) {
        sanitized_format.insert("name".to_string(), Value::String(name.to_string()));
    }

    let mut tuple_schema = None;
    if let Some(Value::Object(schema)) = format.get("schema") {
        let schema = Value::Object(schema.clone());
        let schema = if prepare_tuple_schema {
            let prepared = prepare_schema(schema);
            tuple_schema = prepared.original_schema;
            prepared.schema
        } else {
            schema
        };
        sanitized_format.insert("schema".to_string(), schema);
    }
    if let Some(strict) = format.get("strict").and_then(Value::as_bool) {
        sanitized_format.insert("strict".to_string(), Value::Bool(strict));
    }

    PreparedTextFormat {
        text: Some(json!({"format": sanitized_format})),
        tuple_schema,
    }
}

fn sanitize_responses_input(input: Value) -> Vec<Value> {
    match input {
        Value::Array(items) => sanitize_codex_input_items(items),
        Value::Null => Vec::new(),
        Value::String(text) => vec![responses_input_text_message(&text)],
        value => vec![value],
    }
}

fn sanitize_codex_input_items(input: Vec<Value>) -> Vec<Value> {
    input
        .into_iter()
        .filter_map(|item| {
            if let Value::String(text) = item {
                return Some(responses_input_text_message(&text));
            }
            let Value::Object(object) = item else {
                return Some(item);
            };
            match object.get("type").and_then(Value::as_str) {
                Some("reasoning") => sanitize_reasoning_item(&object),
                Some("compaction") => sanitize_compaction_item(&object),
                _ => Some(Value::Object(object)),
            }
        })
        .collect()
}

fn responses_input_text_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [
            {
                "type": "input_text",
                "text": text
            }
        ]
    })
}

fn sanitize_reasoning_item(item: &Map<String, Value>) -> Option<Value> {
    let id = non_empty_str(item.get("id"))?;
    let summary = sanitize_summary(item.get("summary"))?;
    let mut sanitized = Map::new();
    sanitized.insert("type".to_string(), Value::String("reasoning".to_string()));
    sanitized.insert("id".to_string(), Value::String(id.to_string()));
    sanitized.insert("summary".to_string(), Value::Array(summary));
    if let Some(status) = item
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "in_progress" | "completed" | "incomplete"))
    {
        sanitized.insert("status".to_string(), Value::String(status.to_string()));
    }
    if let Some(encrypted_content) = non_empty_str(item.get("encrypted_content")) {
        sanitized.insert(
            "encrypted_content".to_string(),
            Value::String(encrypted_content.to_string()),
        );
    }
    if let Some(content) = sanitize_reasoning_content(item.get("content")) {
        sanitized.insert("content".to_string(), Value::Array(content));
    }
    Some(Value::Object(sanitized))
}

fn sanitize_summary(value: Option<&Value>) -> Option<Vec<Value>> {
    let Value::Array(parts) = value? else {
        return None;
    };
    Some(
        parts
            .iter()
            .filter_map(|part| {
                let Value::Object(part) = part else {
                    return None;
                };
                if part.get("type").and_then(Value::as_str) != Some("summary_text") {
                    return None;
                }
                let text = part.get("text").and_then(Value::as_str)?;
                Some(json!({"type": "summary_text", "text": text}))
            })
            .collect(),
    )
}

fn sanitize_reasoning_content(value: Option<&Value>) -> Option<Vec<Value>> {
    let Value::Array(parts) = value? else {
        return None;
    };
    let content = parts
        .iter()
        .filter_map(|part| {
            let Value::Object(part) = part else {
                return None;
            };
            if part.get("type").and_then(Value::as_str) != Some("reasoning_text") {
                return None;
            }
            let text = part.get("text").and_then(Value::as_str)?;
            Some(json!({"type": "reasoning_text", "text": text}))
        })
        .collect::<Vec<_>>();
    (!content.is_empty()).then_some(content)
}

fn sanitize_compaction_item(item: &Map<String, Value>) -> Option<Value> {
    let encrypted_content = non_empty_str(item.get("encrypted_content"))?;
    let mut sanitized = Map::new();
    sanitized.insert("type".to_string(), Value::String("compaction".to_string()));
    sanitized.insert(
        "encrypted_content".to_string(),
        Value::String(encrypted_content.to_string()),
    );
    if let Some(id) = non_empty_str(item.get("id")) {
        sanitized.insert("id".to_string(), Value::String(id.to_string()));
    }
    Some(Value::Object(sanitized))
}

fn responses_reasoning(reasoning: Option<Value>) -> Option<Value> {
    let Value::Object(input) = reasoning? else {
        return None;
    };
    let effort = input.get("effort").and_then(Value::as_str);
    let summary = input
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let mut output = Map::new();
    output.insert("summary".to_string(), Value::String(summary.to_string()));
    if let Some(effort) = effort {
        output.insert("effort".to_string(), Value::String(effort.to_string()));
    }
    Some(Value::Object(output))
}

fn compact_reasoning(reasoning: Option<Value>) -> Option<Value> {
    let Value::Object(input) = reasoning? else {
        return None;
    };
    let mut output = Map::new();
    if let Some(effort) = input.get("effort").and_then(Value::as_str) {
        output.insert("effort".to_string(), Value::String(effort.to_string()));
    }
    if let Some(summary) = input.get("summary").and_then(Value::as_str) {
        output.insert("summary".to_string(), Value::String(summary.to_string()));
    }
    (!output.is_empty()).then_some(Value::Object(output))
}

fn sanitize_client_metadata(client_metadata: Option<Value>) -> Option<Value> {
    let Value::Object(input) = client_metadata? else {
        return None;
    };
    let metadata = input
        .into_iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key, Value::String(value.to_string())))
        })
        .collect::<Map<_, _>>();
    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

fn first_request_string(
    direct: Option<String>,
    client_metadata: Option<&Value>,
    metadata_key: &str,
) -> Option<String> {
    non_empty_string(direct).or_else(|| metadata_string(client_metadata, metadata_key))
}

fn metadata_string(client_metadata: Option<&Value>, key: &str) -> Option<String> {
    client_metadata?
        .as_object()?
        .get(key)?
        .as_str()
        .and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
}

fn non_empty_array(value: Option<Value>) -> Option<Vec<Value>> {
    let Value::Array(values) = value? else {
        return None;
    };
    (!values.is_empty()).then_some(values)
}

fn string_array(value: Option<Value>) -> Option<Vec<String>> {
    let Value::Array(values) = value? else {
        return None;
    };
    values
        .into_iter()
        .map(|value| match value {
            Value::String(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn non_empty_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    let value = value?.as_str()?;
    (!value.trim().is_empty()).then_some(value)
}

/// 编码 OpenAI Responses `response.failed` SSE 事件。
pub fn response_failed_sse_event(error_type: &str, code: &str, message: &str) -> String {
    response_failed_sse_event_with_id(None, error_type, code, message)
}

/// 使用指定 response id 编码 OpenAI Responses `response.failed` SSE 事件。
pub fn response_failed_sse_event_with_id(
    response_id: Option<&str>,
    error_type: &str,
    code: &str,
    message: &str,
) -> String {
    let error = json!({
        "type": error_type,
        "code": code,
        "message": message,
    });
    let response_id = response_id
        .filter(|value| !value.trim().is_empty())
        .map_or_else(
            || format!("resp_proxy_{}", uuid::Uuid::new_v4().simple()),
            ToString::to_string,
        );
    let data = json!({
        "type": "response.failed",
        "response": {
            "id": response_id,
            "status": "failed",
            "error": error,
        },
        "error": error,
    });
    encode_sse_event("response.failed", &data.to_string())
}

// ====================================================================
// HTTP 处理器
// ====================================================================

/// `POST /v1/responses`
pub async fn responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(state, request_id, headers, body, "/v1/responses", None).await
}

/// `POST /v1/responses/review`
pub async fn review_responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(
        state,
        request_id,
        headers,
        body,
        "/v1/responses/review",
        Some("review"),
    )
    .await
}

/// `POST /v1/responses/compact`
pub async fn compact_responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_compact_responses(state, request_id, headers, body).await
}

async fn handle_responses(
    state: AppState,
    request_id: RequestId,
    headers: HeaderMap,
    body: Bytes,
    route: &str,
    forced_subagent: Option<&str>,
) -> Response {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    let Ok(mut openai_request) = parse_openai_responses_request(&body) else {
        return invalid_responses_request_response().into_response();
    };
    apply_responses_header_context(&mut openai_request, &headers);
    if openai_request.use_websocket.is_none() {
        openai_request.use_websocket = Some(true);
    }
    if let Some(subagent) = forced_subagent {
        force_openai_subagent(&mut openai_request, subagent);
    } else if let Some(subagent) = openai_subagent_from_headers(&headers) {
        force_openai_subagent(&mut openai_request, &subagent);
    }
    let model = match validated_responses_model(&state, &openai_request.model).await {
        Ok(model) => model,
        Err(ResponsesModelValidationError::InvalidRequest) => {
            return invalid_responses_request_response().into_response();
        }
        Err(ResponsesModelValidationError::ModelNotFound) => {
            return model_not_found_response().into_response();
        }
    };
    let stream = openai_request.stream;
    let mut codex_request = translate_response_to_codex(openai_request);
    attach_client_context(&mut codex_request, &headers);

    if stream {
        return match state
            .services
            .responses
            .stream(request_id.as_str(), route, codex_request, &model)
            .await
        {
            Ok(stream) => live_event_stream_response(stream),
            Err(error) => response_dispatch_stream_error_response(&error),
        };
    }

    match state
        .services
        .responses
        .complete(request_id.as_str(), route, codex_request, &model)
        .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => responses_dispatch_error_response(error),
    }
}

async fn handle_compact_responses(
    state: AppState,
    request_id: RequestId,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    let Ok(openai_request) = parse_openai_responses_request(&body) else {
        return invalid_responses_request_response().into_response();
    };
    let model = match validated_responses_model(&state, &openai_request.model).await {
        Ok(model) => model,
        Err(ResponsesModelValidationError::InvalidRequest) => {
            return invalid_responses_request_response().into_response();
        }
        Err(ResponsesModelValidationError::ModelNotFound) => {
            return model_not_found_response().into_response();
        }
    };
    let mut compact_request = translate_response_to_compact(openai_request);
    compact_request.client_ip = client_ip_from_headers(&headers);
    compact_request.client_user_agent = user_agent_from_headers(&headers);

    match state
        .services
        .responses
        .compact(request_id.as_str(), compact_request, &model)
        .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => responses_compact_dispatch_error_response(error),
    }
}

fn force_openai_subagent(request: &mut OpenAiResponsesRequest, subagent: &str) {
    let mut metadata = match request.client_metadata.take() {
        Some(Value::Object(metadata)) => metadata,
        _ => serde_json::Map::new(),
    };
    metadata.insert(
        OPENAI_SUBAGENT_HEADER.to_string(),
        Value::String(subagent.to_string()),
    );
    request.client_metadata = Some(Value::Object(metadata));
}

fn parse_openai_responses_request(
    body: &Bytes,
) -> Result<OpenAiResponsesRequest, serde_json::Error> {
    serde_json::from_slice(body)
}

async fn validated_responses_model(
    state: &AppState,
    raw_model: &str,
) -> Result<String, ResponsesModelValidationError> {
    let model = raw_model.trim();
    if model.is_empty() {
        return Err(ResponsesModelValidationError::InvalidRequest);
    }
    let catalog = model_catalog_for_state(state).await;
    if catalog.is_recognized_model_name(model) {
        Ok(model.to_string())
    } else {
        Err(ResponsesModelValidationError::ModelNotFound)
    }
}

enum ResponsesModelValidationError {
    InvalidRequest,
    ModelNotFound,
}

fn apply_responses_header_context(request: &mut OpenAiResponsesRequest, headers: &HeaderMap) {
    fill_string_from_header(&mut request.turn_state, headers, "x-codex-turn-state");
    fill_string_from_header(&mut request.turn_metadata, headers, "x-codex-turn-metadata");
    fill_string_from_header(&mut request.beta_features, headers, "x-codex-beta-features");
    fill_string_from_header(
        &mut request.include_timing_metrics,
        headers,
        "x-responsesapi-include-timing-metrics",
    );
    fill_string_from_header(&mut request.version, headers, "version");
    fill_string_from_header(&mut request.codex_window_id, headers, "x-codex-window-id");
    fill_string_from_header(
        &mut request.parent_thread_id,
        headers,
        "x-codex-parent-thread-id",
    );
}

fn fill_string_from_header(field: &mut Option<String>, headers: &HeaderMap, name: &str) {
    if field
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return;
    }
    *field = header_string(headers, name);
}

pub(crate) fn attach_client_context(request: &mut CodexResponsesRequest, headers: &HeaderMap) {
    request.client_ip = client_ip_from_headers(headers);
    request.client_user_agent = user_agent_from_headers(headers);
}

pub(crate) fn user_agent_from_headers(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "user-agent")
}

pub(crate) fn client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "cf-connecting-ip")
        .or_else(|| header_string(headers, "x-real-ip"))
        .or_else(|| forwarded_client_ip(headers))
        .and_then(|value| normalize_client_ip(&value))
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<String> {
    let value = header_string(headers, "x-forwarded-for")?;
    let parts = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts
        .iter()
        .find(|part| !is_private_ip(part))
        .or_else(|| parts.first())
        .map(|part| (*part).to_string())
}

fn normalize_client_ip(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed
        .parse::<std::net::SocketAddr>()
        .map(|addr| addr.ip().to_string())
        .ok()
        .or_else(|| Some(trimmed.to_string()))
}

fn is_private_ip(value: &str) -> bool {
    let Some(ip) = normalize_client_ip(value).and_then(|value| value.parse::<IpAddr>().ok()) else {
        return false;
    };

    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])
        }
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn openai_subagent_from_headers(headers: &HeaderMap) -> Option<String> {
    header_string(headers, OPENAI_SUBAGENT_HEADER).and_then(|value| {
        if matches!(
            value.as_str(),
            "review" | "compact" | "memory_consolidation" | "collab_spawn"
        ) {
            Some(value)
        } else {
            None
        }
    })
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn event_stream_response(mut body: String) -> Response {
    ensure_done_sse_frame(&mut body);
    sse_event_stream_response(Body::from(body), SseResponseOptions::BASIC)
}

fn live_event_stream_response(stream: ResponseDispatchStream) -> Response {
    sse_event_stream_response(Body::from_stream(stream.body), SseResponseOptions::BASIC)
}

fn response_dispatch_stream_error_response(error: &ResponseDispatchError) -> Response {
    event_stream_response(responses_stream_dispatch_failed_sse_event(error))
}

fn ensure_done_sse_frame(body: &mut String) {
    if sse_body_has_done(body) {
        return;
    }
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(done_sse_frame());
}
