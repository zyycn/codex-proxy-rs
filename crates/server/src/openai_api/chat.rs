//! OpenAI 聊天处理器。

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
use codex_proxy_core::{
    models::catalog::ModelCatalog,
    protocol::openai::chat::{
        translate_chat_to_codex, ChatCompletionRequest, ChatCompletionStreamTranslator,
    },
    serving::responses::apply_response_model_options,
};
use codex_proxy_runtime::{
    services::{ChatDispatchError, ResponseDispatchError, ResponseDispatchStream},
    state::AppState,
};
use futures::{stream as futures_stream, StreamExt};
use serde_json::{json, Value};
use std::convert::Infallible;

use crate::middleware::request_id::RequestId;

use super::{auth::authorize_client_api_key, models::model_catalog_for_state};

/// `POST /v1/chat/completions`
pub async fn chat_completions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    let Ok(chat_request) = serde_json::from_slice::<ChatCompletionRequest>(&body) else {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Invalid chat completion request",
            "invalid_request_error",
            "invalid_request",
        )
        .into_response();
    };
    let model = chat_request.model.clone();
    let catalog = model_catalog_for_state(&state).await;
    if !catalog.is_recognized_model_name(&model) {
        return model_not_found_response().into_response();
    }
    let parsed_model = catalog.parse_model_name(&model);
    let display_model = ModelCatalog::build_display_model_name(&parsed_model);
    let stream = chat_request.stream;
    let mut codex_request = match translate_chat_to_codex(chat_request) {
        Ok(request) => request,
        Err(_) => {
            return openai_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid chat completion request",
                "invalid_request_error",
                "invalid_request",
            )
            .into_response();
        }
    };
    apply_response_model_options(
        &mut codex_request,
        &parsed_model,
        state.services.models.config(),
    );
    let include_reasoning = codex_request
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .is_some_and(|effort| !effort.trim().is_empty());
    let tuple_schema = codex_request.tuple_schema.clone();

    if stream {
        return match state
            .services
            .responses
            .stream(
                request_id.as_str(),
                "/v1/chat/completions",
                codex_request,
                &model,
            )
            .await
        {
            Ok(stream) => live_chat_event_stream_response(
                stream,
                &display_model,
                include_reasoning,
                tuple_schema,
            ),
            Err(error) => response_dispatch_chat_stream_error_response(error),
        };
    }

    match state
        .services
        .chat
        .complete(request_id.as_str(), codex_request, &model)
        .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(ChatDispatchError::NoActiveAccount | ChatDispatchError::AccountStore) => {
            openai_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "No active upstream account is available",
                "server_error",
                "upstream_unavailable",
            )
            .into_response()
        }
        Err(ChatDispatchError::Upstream(_)) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            "Upstream Codex request failed",
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::QuotaExhausted {
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
        Err(ChatDispatchError::RateLimited {
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
        Err(ChatDispatchError::Expired {
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
        Err(ChatDispatchError::Disabled {
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
        Err(ChatDispatchError::Banned {
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
        Err(ChatDispatchError::CloudflareChallenge {
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
        Err(ChatDispatchError::CloudflarePathBlocked {
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
        Err(ChatDispatchError::ModelUnsupported {
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
        Err(ChatDispatchError::InvalidSse(_) | ChatDispatchError::EmptyUpstreamResponse) => {
            openai_error_response(
                StatusCode::BAD_GATEWAY,
                "Invalid upstream Codex response",
                "server_error",
                "invalid_upstream_response",
            )
            .into_response()
        }
    }
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

fn live_chat_event_stream_response(
    stream: ResponseDispatchStream,
    model: &str,
    include_reasoning: bool,
    tuple_schema: Option<Value>,
) -> Response {
    let mut translator =
        ChatCompletionStreamTranslator::new(model.to_string(), include_reasoning, tuple_schema);
    let initial_frame = translator.initial_frame();
    let body_stream =
        futures_stream::once(async move { Ok::<Bytes, Infallible>(Bytes::from(initial_frame)) })
            .chain(stream.body.map(move |result| {
                let body = match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        translator
                            .push_str(&text)
                            .unwrap_or_else(|error| chat_stream_error_sse_frame(&error.to_string()))
                    }
                    Err(error) => chat_stream_error_sse_frame(&error.to_string()),
                };
                Ok::<Bytes, Infallible>(Bytes::from(body))
            }));

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(body_stream))
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

fn response_dispatch_chat_stream_error_response(error: ResponseDispatchError) -> Response {
    let message = match error {
        ResponseDispatchError::NoActiveAccount | ResponseDispatchError::AccountStore => {
            "No active upstream account is available".to_string()
        }
        ResponseDispatchError::Upstream(_) => "Upstream Codex request failed".to_string(),
        ResponseDispatchError::QuotaExhausted {
            count,
            upstream_error,
        } => format!(
            "All accounts exhausted ({count} quota-exhausted). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::RateLimited {
            count,
            upstream_error,
        } => format!(
            "All accounts exhausted ({count} rate-limited). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::Expired {
            count,
            upstream_error,
        } => format!(
            "All accounts exhausted ({count} expired). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::Disabled {
            count,
            upstream_error,
        } => format!(
            "All accounts exhausted ({count} disabled). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::Banned {
            count,
            upstream_error,
            ..
        } => format!(
            "All accounts exhausted ({count} banned). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        } => format!(
            "All accounts exhausted ({count} cloudflare-challenge). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        } => format!(
            "All accounts exhausted ({count} cloudflare-path-block). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::ModelUnsupported {
            count,
            upstream_error,
        } => format!(
            "All accounts exhausted ({count} model-unsupported). Codex upstream error: {upstream_error}"
        ),
        ResponseDispatchError::InvalidSse(_)
        | ResponseDispatchError::MissingCompleted
        | ResponseDispatchError::EmptyUpstreamResponse => "Invalid upstream Codex response".to_string(),
        ResponseDispatchError::Failed(_) => "Upstream Codex response failed".to_string(),
    };
    chat_stream_error_response(&message)
}

fn chat_stream_error_response(message: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .header("connection", "keep-alive")
        .body(Body::from(chat_stream_error_sse_frame(message)))
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

fn chat_stream_error_sse_frame(message: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({
            "error": {
                "message": message,
                "type": "stream_error",
            }
        })
    )
}
