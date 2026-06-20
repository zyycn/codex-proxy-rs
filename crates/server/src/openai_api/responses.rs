//! OpenAI Responses 处理器。

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use codex_proxy_core::protocol::{
    codex::sse::sse_body_has_done,
    openai::responses::{
        translate_response_to_codex, translate_response_to_compact, OpenAiResponsesRequest,
    },
};
use codex_proxy_runtime::{
    services::{ResponseDispatchError, ResponseDispatchStream},
    state::AppState,
};
use serde_json::Value;

use crate::middleware::request_id::RequestId;

use super::{
    auth::authorize_client_api_key,
    error::{
        invalid_responses_request_response, missing_client_api_key_response,
        model_not_found_response, responses_compact_dispatch_error_response,
        responses_dispatch_error_response, responses_stream_dispatch_failed_sse_event,
    },
    models::model_catalog_for_state,
    sse::{done_sse_frame, event_stream_response as sse_event_stream_response, SseResponseOptions},
};

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

    let Ok(mut openai_request) = parse_openai_responses_request(&body) else {
        return invalid_responses_request_response().into_response();
    };
    apply_responses_header_context(&mut openai_request, &headers);
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
    let compact_request = translate_response_to_compact(openai_request);

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

fn response_dispatch_stream_error_response(error: ResponseDispatchError) -> Response {
    event_stream_response(responses_stream_dispatch_failed_sse_event(&error))
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
