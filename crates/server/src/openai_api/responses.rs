//! OpenAI Responses 处理器。

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use codex_proxy_core::protocol::openai::responses::{
    translate_response_to_codex, translate_response_to_compact, OpenAiResponsesRequest,
};
use codex_proxy_runtime::{
    services::{ResponseDispatchError, ResponseDispatchStream},
    state::AppState,
};
use serde_json::{json, Value};

use crate::middleware::request_id::RequestId;

use super::{
    auth::authorize_client_api_key,
    models::model_catalog_for_state,
    sse::{done_sse_frame, openai_sse_frame},
};

const OPENAI_SUBAGENT_METADATA_KEY: &str = "x-openai-subagent";
const OPENAI_SUBAGENT_HEADER: &str = "x-openai-subagent";

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

    let Ok(mut openai_request) = serde_json::from_slice::<OpenAiResponsesRequest>(&body) else {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Invalid responses request",
            "invalid_request_error",
            "invalid_request",
        )
        .into_response();
    };
    apply_responses_header_context(&mut openai_request, &headers);
    if let Some(subagent) = forced_subagent {
        force_openai_subagent(&mut openai_request, subagent);
    } else if let Some(subagent) = openai_subagent_from_headers(&headers) {
        force_openai_subagent(&mut openai_request, &subagent);
    }
    let model = openai_request.model.trim().to_string();
    if model.is_empty() {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Invalid responses request",
            "invalid_request_error",
            "invalid_request",
        )
        .into_response();
    };
    let catalog = model_catalog_for_state(&state).await;
    if !catalog.is_recognized_model_name(&model) {
        return model_not_found_response().into_response();
    }
    let stream = openai_request.stream;
    let codex_request = translate_response_to_codex(openai_request);

    if stream {
        return match state
            .services
            .responses
            .stream(request_id.as_str(), route, codex_request, &model)
            .await
        {
            Ok(stream) => live_event_stream_response(stream),
            Err(error) => response_dispatch_stream_error_response(error),
        };
    }

    match state
        .services
        .responses
        .complete(request_id.as_str(), route, codex_request, &model)
        .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(ResponseDispatchError::NoActiveAccount | ResponseDispatchError::AccountStore) => {
            responses_no_available_accounts_response().into_response()
        }
        Err(ResponseDispatchError::Upstream(_)) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            "Upstream Codex request failed",
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::QuotaExhausted {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::PAYMENT_REQUIRED,
            &format!(
                "All accounts exhausted ({count} quota-exhausted). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::RateLimited {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "All accounts exhausted ({count} rate-limited). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::Expired {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::UNAUTHORIZED,
            &format!(
                "All accounts exhausted ({count} expired). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::Disabled {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::UNAUTHORIZED,
            &format!(
                "All accounts exhausted ({count} disabled). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::Banned {
            count,
            upstream_error,
            status_code,
        }) => openai_error_response(
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::FORBIDDEN),
            &format!(
                "All accounts exhausted ({count} banned). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            &format!(
                "All accounts exhausted ({count} cloudflare-challenge). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            &format!(
                "All accounts exhausted ({count} cloudflare-path-block). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ResponseDispatchError::ModelUnsupported {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::BAD_REQUEST,
            &format!(
                "All accounts exhausted ({count} model-unsupported). Codex upstream error: {upstream_error}"
            ),
            "invalid_request_error",
            "upstream_error",
        )
        .into_response(),
        Err(
            ResponseDispatchError::InvalidSse(_)
            | ResponseDispatchError::MissingCompleted
            | ResponseDispatchError::EmptyUpstreamResponse,
        ) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream Codex response",
            "server_error",
            "invalid_upstream_response",
        )
        .into_response(),
        Err(ResponseDispatchError::Failed(_)) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            "Upstream Codex response failed",
            "server_error",
            "upstream_error",
        )
        .into_response(),
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

    let Ok(openai_request) = serde_json::from_slice::<OpenAiResponsesRequest>(&body) else {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Invalid responses request",
            "invalid_request_error",
            "invalid_request",
        )
        .into_response();
    };
    let model = openai_request.model.trim().to_string();
    if model.is_empty() {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Invalid responses request",
            "invalid_request_error",
            "invalid_request",
        )
        .into_response();
    };
    let catalog = model_catalog_for_state(&state).await;
    if !catalog.is_recognized_model_name(&model) {
        return model_not_found_response().into_response();
    }
    let compact_request = translate_response_to_compact(openai_request);

    match state
        .services
        .responses
        .compact(request_id.as_str(), compact_request, &model)
        .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => response_dispatch_compact_error_response(error),
    }
}

fn force_openai_subagent(request: &mut OpenAiResponsesRequest, subagent: &str) {
    let mut metadata = match request.client_metadata.take() {
        Some(Value::Object(metadata)) => metadata,
        _ => serde_json::Map::new(),
    };
    metadata.insert(
        OPENAI_SUBAGENT_METADATA_KEY.to_string(),
        Value::String(subagent.to_string()),
    );
    request.client_metadata = Some(Value::Object(metadata));
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

fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::UNAUTHORIZED,
        "Missing client API key",
        "invalid_request_error",
        "invalid_api_key",
    )
}

fn model_not_found_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::NOT_FOUND,
        "Model not found",
        "invalid_request_error",
        "model_not_found",
    )
}

fn responses_no_available_accounts_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "type": "error",
            "error": {
                "type": "server_error",
                "code": "no_available_accounts",
                "message": "No available accounts. All accounts are expired or rate-limited.",
            }
        })),
    )
}

fn responses_dispatch_error_response(
    status: StatusCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let (error_type, code) = if status == StatusCode::TOO_MANY_REQUESTS {
        ("rate_limit_error", "rate_limit_exceeded")
    } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        ("invalid_request_error", "authentication_error")
    } else if status.is_client_error() {
        ("invalid_request_error", "codex_api_error")
    } else {
        ("server_error", "codex_api_error")
    };
    (
        status,
        Json(json!({
            "type": "error",
            "error": {
                "type": error_type,
                "code": code,
                "message": message,
            }
        })),
    )
}

fn openai_error_response(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": code
            }
        })),
    )
}

fn response_dispatch_compact_error_response(error: ResponseDispatchError) -> Response {
    match error {
        ResponseDispatchError::NoActiveAccount | ResponseDispatchError::AccountStore => {
            responses_no_available_accounts_response().into_response()
        }
        ResponseDispatchError::Upstream(_) => responses_dispatch_error_response(
            StatusCode::BAD_GATEWAY,
            "Upstream Codex request failed",
        )
        .into_response(),
        ResponseDispatchError::QuotaExhausted {
            count,
            upstream_error,
        } => responses_dispatch_error_response(
            StatusCode::PAYMENT_REQUIRED,
            &format!(
                "All accounts exhausted ({count} quota-exhausted). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::RateLimited {
            count,
            upstream_error,
        } => responses_dispatch_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "All accounts exhausted ({count} rate-limited). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::Expired {
            count,
            upstream_error,
        } => responses_dispatch_error_response(
            StatusCode::UNAUTHORIZED,
            &format!(
                "All accounts exhausted ({count} expired). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::Disabled {
            count,
            upstream_error,
        } => responses_dispatch_error_response(
            StatusCode::UNAUTHORIZED,
            &format!(
                "All accounts exhausted ({count} disabled). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::Banned {
            count,
            upstream_error,
            status_code,
        } => responses_dispatch_error_response(
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::FORBIDDEN),
            &format!(
                "All accounts exhausted ({count} banned). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        } => responses_dispatch_error_response(
            StatusCode::BAD_GATEWAY,
            &format!(
                "All accounts exhausted ({count} cloudflare-challenge). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        } => responses_dispatch_error_response(
            StatusCode::BAD_GATEWAY,
            &format!(
                "All accounts exhausted ({count} cloudflare-path-block). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::ModelUnsupported {
            count,
            upstream_error,
        } => responses_dispatch_error_response(
            StatusCode::BAD_REQUEST,
            &format!(
                "All accounts exhausted ({count} model-unsupported). Codex upstream error: {upstream_error}"
            ),
        )
        .into_response(),
        ResponseDispatchError::InvalidSse(_)
        | ResponseDispatchError::MissingCompleted
        | ResponseDispatchError::EmptyUpstreamResponse => responses_dispatch_error_response(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream Codex response",
        )
        .into_response(),
        ResponseDispatchError::Failed(_) => responses_dispatch_error_response(
            StatusCode::BAD_GATEWAY,
            "Upstream Codex response failed",
        )
        .into_response(),
    }
}

fn event_stream_response(mut body: String) -> Response {
    ensure_done_sse_frame(&mut body);
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            openai_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build stream response",
                "server_error",
                "stream_response_error",
            )
            .into_response()
        })
}

fn live_event_stream_response(stream: ResponseDispatchStream) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream.body))
        .unwrap_or_else(|_| {
            openai_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build stream response",
                "server_error",
                "stream_response_error",
            )
            .into_response()
        })
}

fn response_dispatch_stream_error_response(error: ResponseDispatchError) -> Response {
    let (status, message) = match error {
        ResponseDispatchError::NoActiveAccount | ResponseDispatchError::AccountStore => (
            StatusCode::SERVICE_UNAVAILABLE,
            "No active upstream account is available".to_string(),
        ),
        ResponseDispatchError::Upstream(_) => (
            StatusCode::BAD_GATEWAY,
            "Upstream Codex request failed".to_string(),
        ),
        ResponseDispatchError::QuotaExhausted {
            count,
            upstream_error,
        } => (
            StatusCode::PAYMENT_REQUIRED,
            format!(
                "All accounts exhausted ({count} quota-exhausted). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::RateLimited {
            count,
            upstream_error,
        } => (
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "All accounts exhausted ({count} rate-limited). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::Expired {
            count,
            upstream_error,
        } => (
            StatusCode::UNAUTHORIZED,
            format!(
                "All accounts exhausted ({count} expired). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::Disabled {
            count,
            upstream_error,
        } => (
            StatusCode::UNAUTHORIZED,
            format!(
                "All accounts exhausted ({count} disabled). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::Banned {
            count,
            upstream_error,
            status_code,
        } => (
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::FORBIDDEN),
            format!(
                "All accounts exhausted ({count} banned). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        } => (
            StatusCode::BAD_GATEWAY,
            format!(
                "All accounts exhausted ({count} cloudflare-challenge). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        } => (
            StatusCode::BAD_GATEWAY,
            format!(
                "No accounts available. All accounts exhausted ({count} cloudflare-path-block). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::ModelUnsupported {
            count,
            upstream_error,
        } => (
            StatusCode::BAD_REQUEST,
            format!(
                "No accounts available. All accounts exhausted ({count} model-unsupported). Codex upstream error: {upstream_error}"
            ),
        ),
        ResponseDispatchError::InvalidSse(_)
        | ResponseDispatchError::MissingCompleted
        | ResponseDispatchError::EmptyUpstreamResponse => (
            StatusCode::BAD_GATEWAY,
            "Invalid upstream Codex response".to_string(),
        ),
        ResponseDispatchError::Failed(_) => (
            StatusCode::BAD_GATEWAY,
            "Upstream Codex response failed".to_string(),
        ),
    };
    event_stream_response(response_failed_sse_event(status, &message))
}

fn ensure_done_sse_frame(body: &mut String) {
    if body
        .trim_end_matches(['\r', '\n'])
        .ends_with("data: [DONE]")
    {
        return;
    }
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(done_sse_frame());
}

fn response_failed_sse_event(status: StatusCode, message: &str) -> String {
    let (error_type, code) = if status == StatusCode::TOO_MANY_REQUESTS {
        ("rate_limit_error", "rate_limit_exceeded")
    } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        ("invalid_request_error", "authentication_error")
    } else if status.is_client_error() {
        ("invalid_request_error", "codex_api_error")
    } else {
        ("server_error", "codex_api_error")
    };
    let error = json!({
        "type": error_type,
        "code": code,
        "message": message,
    });
    let data = json!({
        "type": "response.failed",
        "response": {
            "id": format!("resp_proxy_{}", uuid::Uuid::new_v4().simple()),
            "status": "failed",
            "error": error,
        },
        "error": error,
    });
    openai_sse_frame("response.failed", &data.to_string())
}
