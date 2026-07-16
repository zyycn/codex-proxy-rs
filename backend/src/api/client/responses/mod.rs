//! OpenAI Responses API 透明代理入口：解析原始 JSON body，提取调度元数据，
//! 做最小 patch 后交给调度层，请求语义原样透传上游。

mod sse;
mod websocket;

pub use websocket::responses_websocket;

use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value};

use crate::{
    api::AppState,
    api::middleware::request_id::{ClientIp, RequestId},
    dispatch::{errors::ResponseDispatchError, service::ResponseDispatchStream},
    upstream::openai::protocol::{
        responses::{
            CodexResponsesRequest, WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY,
            X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER, X_OPENAI_MEMGEN_REQUEST_HEADER,
        },
        sse::sse_body_has_done,
    },
};

use self::sse::{done_sse_frame, event_stream_response as sse_event_stream_response};
use super::{
    errors::{
        invalid_responses_request_response, missing_client_api_key_response,
        model_not_found_response, responses_dispatch_error_response,
        responses_dispatch_error_response_ref, responses_stream_dispatch_failed_sse_event,
    },
    models::model_catalog_for_state,
};

const OPENAI_SUBAGENT_HEADER: &str = "x-openai-subagent";

/// 代理内部 transport 提示字段：用于本地传输决策，不转发上游。
const TRANSPORT_ONLY_KEYS: [&str; 1] = ["use_websocket"];

/// Codex 上下文字段：从 body 顶层（camelCase）提取到代理控制状态用于生成请求头，
/// 同时保留在 body 中原样转发上游。
const CONTEXT_HEADER_FIELDS: [(&str, ContextField); 7] = [
    ("turnState", ContextField::TurnState),
    ("turnMetadata", ContextField::TurnMetadata),
    ("betaFeatures", ContextField::BetaFeatures),
    ("version", ContextField::Version),
    ("includeTimingMetrics", ContextField::IncludeTimingMetrics),
    ("codexWindowId", ContextField::CodexWindowId),
    ("parentThreadId", ContextField::ParentThreadId),
];

#[derive(Clone, Copy)]
enum ContextField {
    TurnState,
    TurnMetadata,
    BetaFeatures,
    Version,
    IncludeTimingMetrics,
    CodexWindowId,
    ParentThreadId,
}

// ====================================================================
// 透明代理请求构造
// ====================================================================

/// 从客户端原始 Responses JSON body 构造上游请求（透明代理）。
///
/// 只做代理职责范围内的最小处理：
/// - 剥离 transport-only 字段（`use_websocket`），仅用于本地传输决策。
/// - 从 body 与请求头提取 Codex 上下文透传字段到代理控制状态（body 中保留原值）。
/// - review route / 合法请求头时，往 `client_metadata` 注入 forced subagent。
///
/// 其余字段——`input`、`reasoning`、`text`、`tools`、`tool_choice`、`include`、
/// `client_metadata`、`service_tier` 以及一切未知顶层字段——全部原样透传，不重写。
pub fn build_codex_request(
    mut body: Map<String, Value>,
    headers: &HeaderMap,
    forced_subagent: Option<&str>,
) -> CodexResponsesRequest {
    let use_websocket = body.get("use_websocket").and_then(Value::as_bool);
    for key in TRANSPORT_ONLY_KEYS {
        body.remove(key);
    }

    if let Some(subagent) = forced_subagent
        .map(ToString::to_string)
        .or_else(|| openai_subagent_from_headers(headers))
    {
        inject_subagent_metadata(&mut body, &subagent);
    }

    let explicit_prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());

    let mut request = CodexResponsesRequest::from_body(body);
    request.explicit_prompt_cache_key = explicit_prompt_cache_key;
    match use_websocket {
        Some(true) => request.use_websocket = true,
        Some(false) => request.force_http_sse = true,
        None => {}
    }
    apply_context_header_fields(&mut request, headers);
    apply_request_semantic_headers(&mut request, headers);
    apply_session_fields(&mut request, headers);
    request.client_user_agent = user_agent_from_headers(headers);
    request
}

fn apply_request_semantic_headers(request: &mut CodexResponsesRequest, headers: &HeaderMap) {
    request.responses_lite = header_string(headers, X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER)
        .or_else(|| {
            request
                .client_metadata()
                .and_then(Value::as_object)
                .and_then(|metadata| {
                    metadata.get(WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY)
                })
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        });
    request.memgen_request = header_string(headers, X_OPENAI_MEMGEN_REQUEST_HEADER);
}

fn apply_session_fields(request: &mut CodexResponsesRequest, headers: &HeaderMap) {
    request.client_conversation_id = session_context_string(request.body(), "conversation_id")
        .or_else(|| header_string(headers, "conversation-id"))
        .or_else(|| header_string(headers, "conversation_id"))
        .or_else(|| request.prompt_cache_key().map(ToString::to_string));
    request.client_session_id = session_context_string(request.body(), "session_id")
        .or_else(|| header_string(headers, "session-id"))
        .or_else(|| header_string(headers, "session_id"));
    request.client_thread_id = session_context_string(request.body(), "thread_id")
        .or_else(|| header_string(headers, "thread-id"));
    request.client_request_id = session_context_string(request.body(), "x-client-request-id")
        .or_else(|| header_string(headers, "x-client-request-id"));
    request.client_turn_id = session_context_string(request.body(), "turn_id")
        .or_else(|| header_string(headers, "x-codex-turn-id"));
    request.codex_window_id = request.codex_window_id.take().or_else(|| {
        session_context_string(request.body(), "x-codex-window-id")
            .or_else(|| header_string(headers, "x-codex-window-id"))
    });
    request.parent_thread_id = request.parent_thread_id.take().or_else(|| {
        session_context_string(request.body(), "x-codex-parent-thread-id")
            .or_else(|| header_string(headers, "x-codex-parent-thread-id"))
    });
}

fn session_context_string(body: &Map<String, Value>, key: &str) -> Option<String> {
    body_context_string(body, key).or_else(|| {
        body.get("client_metadata")
            .and_then(Value::as_object)
            .and_then(|metadata| metadata.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn inject_subagent_metadata(body: &mut Map<String, Value>, subagent: &str) {
    let metadata = body
        .entry("client_metadata")
        .or_insert_with(|| Value::Object(Map::new()));
    if !metadata.is_object() {
        *metadata = Value::Object(Map::new());
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            OPENAI_SUBAGENT_HEADER.to_string(),
            Value::String(subagent.to_string()),
        );
    }
}

/// 从 body 与请求头提取 Codex 上下文透传字段到代理控制状态。
///
/// body 中的原值优先；缺失时回退请求头。提取只填充代理控制状态用于加请求头，
/// **不修改 body**——这些字段在 body 中原样保留转发上游。
fn apply_context_header_fields(request: &mut CodexResponsesRequest, headers: &HeaderMap) {
    for (body_key, field) in CONTEXT_HEADER_FIELDS {
        let value = body_context_string(request.body(), body_key)
            .or_else(|| header_string(headers, field.header_name()));
        field.assign(request, value);
    }
}

fn body_context_string(body: &Map<String, Value>, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

impl ContextField {
    fn header_name(self) -> &'static str {
        match self {
            Self::TurnState => "x-codex-turn-state",
            Self::TurnMetadata => "x-codex-turn-metadata",
            Self::BetaFeatures => "x-codex-beta-features",
            Self::Version => "version",
            Self::IncludeTimingMetrics => "x-responsesapi-include-timing-metrics",
            Self::CodexWindowId => "x-codex-window-id",
            Self::ParentThreadId => "x-codex-parent-thread-id",
        }
    }

    fn assign(self, request: &mut CodexResponsesRequest, value: Option<String>) {
        match self {
            Self::TurnState => request.turn_state = value,
            Self::TurnMetadata => request.turn_metadata = value,
            Self::BetaFeatures => request.beta_features = value,
            Self::Version => request.version = value,
            Self::IncludeTimingMetrics => request.include_timing_metrics = value,
            Self::CodexWindowId => request.codex_window_id = value,
            Self::ParentThreadId => request.parent_thread_id = value,
        }
    }
}

// ====================================================================
// HTTP 处理器
// ====================================================================

/// `POST /v1/responses`
pub async fn responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(
        state,
        request_id,
        client_ip.map(|Extension(client_ip)| client_ip.as_str().to_string()),
        headers,
        body,
        "/v1/responses",
        None,
    )
    .await
}

/// `POST /v1/responses/review`
pub async fn review_responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(
        state,
        request_id,
        client_ip.map(|Extension(client_ip)| client_ip.as_str().to_string()),
        headers,
        body,
        "/v1/responses/review",
        Some("review"),
    )
    .await
}

async fn handle_responses(
    state: AppState,
    request_id: RequestId,
    client_ip: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    route: &str,
    forced_subagent: Option<&str>,
) -> Response {
    let Some(client_api_key_id) =
        crate::api::client::auth::authorized_client_api_key_id(&state, &headers).await
    else {
        return missing_client_api_key_response().into_response();
    };

    let Some(body) = parse_responses_body(&body) else {
        return invalid_responses_request_response().into_response();
    };
    let prepared = match prepare_responses_request(
        &state,
        body,
        &headers,
        forced_subagent,
        client_ip,
        client_api_key_id,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(ResponsesRequestValidationError::InvalidRequest) => {
            return invalid_responses_request_response().into_response();
        }
        Err(ResponsesRequestValidationError::ModelNotFound) => {
            return model_not_found_response().into_response();
        }
    };
    let PreparedResponsesRequest {
        mut request,
        requested_model,
    } = prepared;
    // 客户端未显式指定 transport 时默认偏好 WebSocket。
    if !request.force_http_sse {
        request.use_websocket = true;
    }
    let stream = request.stream();

    if stream {
        return match state
            .services
            .responses
            .stream(request_id.as_str(), route, request, &requested_model)
            .await
        {
            Ok(stream) => live_event_stream_response(stream),
            Err(error) => response_dispatch_stream_error_response(&error),
        };
    }

    match state
        .services
        .responses
        .complete(request_id.as_str(), route, request, &requested_model)
        .await
    {
        Ok(result) => apply_safe_response_headers(
            (StatusCode::OK, Json(result.body)).into_response(),
            result.response_headers,
        ),
        Err(error) => responses_dispatch_error_response(error),
    }
}

/// 解析请求体：必须是 JSON object，否则视为 invalid request。
fn parse_responses_body(body: &Bytes) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn request_model(body: &Map<String, Value>) -> &str {
    body.get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

pub(super) struct PreparedResponsesRequest {
    pub(super) request: CodexResponsesRequest,
    pub(super) requested_model: String,
}

pub(super) async fn prepare_responses_request(
    state: &AppState,
    body: Map<String, Value>,
    headers: &HeaderMap,
    forced_subagent: Option<&str>,
    client_ip: Option<String>,
    client_api_key_id: String,
) -> Result<PreparedResponsesRequest, ResponsesRequestValidationError> {
    let requested_model = validated_responses_model(state, request_model(&body)).await?;
    let mut request = build_codex_request(body, headers, forced_subagent);
    request.client_ip = client_ip;
    request.client_api_key_id = Some(client_api_key_id);
    Ok(PreparedResponsesRequest {
        request,
        requested_model,
    })
}

async fn validated_responses_model(
    state: &AppState,
    raw_model: &str,
) -> Result<String, ResponsesRequestValidationError> {
    let model = raw_model.trim();
    if model.is_empty() {
        return Err(ResponsesRequestValidationError::InvalidRequest);
    }
    let catalog = model_catalog_for_state(state).await;
    if catalog.is_empty() || catalog.is_recognized_model_name(model) {
        Ok(model.to_string())
    } else {
        Err(ResponsesRequestValidationError::ModelNotFound)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResponsesRequestValidationError {
    InvalidRequest,
    ModelNotFound,
}

pub(crate) fn user_agent_from_headers(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "user-agent")
}

fn openai_subagent_from_headers(headers: &HeaderMap) -> Option<String> {
    header_string(headers, OPENAI_SUBAGENT_HEADER).filter(|value| {
        matches!(
            value.as_str(),
            "review" | "compact" | "memory_consolidation" | "collab_spawn"
        )
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
    sse_event_stream_response(Body::from(body))
}

fn live_event_stream_response(stream: ResponseDispatchStream) -> Response {
    let response = sse_event_stream_response(Body::from_stream(stream.body));
    apply_safe_response_headers(response, stream.response_headers)
}

fn apply_safe_response_headers(mut response: Response, headers: Vec<(String, String)>) -> Response {
    for (name, value) in headers {
        let name = match name.as_str() {
            "x-request-id" => axum::http::header::HeaderName::from_static("x-request-id"),
            "openai-model" => axum::http::header::HeaderName::from_static("openai-model"),
            "x-models-etag" => axum::http::header::HeaderName::from_static("x-models-etag"),
            "x-reasoning-included" => {
                axum::http::header::HeaderName::from_static("x-reasoning-included")
            }
            "openai-processing-ms" => {
                axum::http::header::HeaderName::from_static("openai-processing-ms")
            }
            _ => continue,
        };
        let Ok(value) = axum::http::HeaderValue::from_str(&value) else {
            continue;
        };
        response.headers_mut().insert(name, value);
    }
    response
}

fn response_dispatch_stream_error_response(error: &ResponseDispatchError) -> Response {
    if (400..=499).contains(&error.client_http_status_code()) {
        return responses_dispatch_error_response_ref(error);
    }
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
