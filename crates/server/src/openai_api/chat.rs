//! OpenAI 聊天处理器。

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
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
    services::{ResponseDispatchError, ResponseDispatchStream},
    state::AppState,
};
use futures::{stream as futures_stream, StreamExt};
use serde_json::{json, Value};
use std::convert::Infallible;

use crate::middleware::request_id::RequestId;

use super::{
    auth::authorize_client_api_key,
    error::{
        chat_dispatch_error_response, chat_stream_dispatch_error_message,
        invalid_chat_completion_request_response, missing_client_api_key_response,
        model_not_found_response,
    },
    models::model_catalog_for_state,
    sse::{event_stream_response, openai_sse_frame, SseResponseOptions},
};

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
        return invalid_chat_completion_request_response().into_response();
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
            return invalid_chat_completion_request_response().into_response();
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
        Err(error) => chat_dispatch_error_response(error),
    }
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

    event_stream_response(
        Body::from_stream(body_stream),
        SseResponseOptions::LIVE_CHAT,
    )
}

fn response_dispatch_chat_stream_error_response(error: ResponseDispatchError) -> Response {
    let message = chat_stream_dispatch_error_message(&error);
    chat_stream_error_response(&message)
}

fn chat_stream_error_response(message: &str) -> Response {
    event_stream_response(
        Body::from(chat_stream_error_sse_frame(message)),
        SseResponseOptions::CHAT_ERROR,
    )
}

fn chat_stream_error_sse_frame(message: &str) -> String {
    openai_sse_frame(
        "",
        &json!({
            "error": {
                "message": message,
                "type": "stream_error",
            }
        })
        .to_string(),
    )
}
