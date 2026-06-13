use std::time::Instant;

use axum::{
    body::Body,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::{
    codex::accounts::models::catalog::ModelCatalog,
    codex::accounts::models::service::ModelService,
    codex::gateway::protocol::{
        codex_to_openai::{
            chat_completion_from_codex_sse, chat_completion_stream_from_codex_sse, openai_error,
        },
        openai_to_codex::{translate_chat_to_codex, ChatCompletionRequest},
    },
    codex::logs::event::EventLevel,
    codex::serving::dispatch::{
        classify_upstream_account_retry, no_available_accounts_response,
        normalize_service_tier_for_upstream, CodexRequestLogContext, CodexUpstreamService,
    },
    codex::serving::http::errors::{codex_client_error_response, model_not_found_response},
    config::ModelConfig,
};

#[derive(Clone)]
pub struct ChatService {
    model_config: ModelConfig,
    models: ModelService,
    upstream: CodexUpstreamService,
}

impl ChatService {
    pub(crate) fn new(
        model_config: ModelConfig,
        models: ModelService,
        upstream: CodexUpstreamService,
    ) -> Self {
        Self {
            model_config,
            models,
            upstream,
        }
    }

    pub async fn handle(
        &self,
        request_id: &str,
        chat_request: ChatCompletionRequest,
        started_at: Instant,
    ) -> Response {
        let client_stream = chat_request.stream;
        let requested_model = chat_request.model.clone();
        let catalog = self.models.catalog().await;
        if !catalog.is_recognized_model_name(&requested_model) {
            return model_not_found_response().into_response();
        }
        let parsed_model = catalog.parse_model_name(&requested_model);
        let display_model = ModelCatalog::build_display_model_name(&parsed_model);
        let mut codex_request = match translate_chat_to_codex(chat_request) {
            Ok(request) => request,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(openai_error(
                        "Invalid chat completion request",
                        "invalid_request",
                    )),
                )
                    .into_response();
            }
        };
        codex_request.model = parsed_model.model_id.clone();
        if codex_request.reasoning.is_none() {
            let effort = parsed_model
                .reasoning_effort
                .clone()
                .or_else(|| self.model_config.default_reasoning_effort.clone());
            if let Some(effort) = effort {
                codex_request.reasoning = Some(json!({"effort": effort, "summary": "auto"}));
            }
        }
        if codex_request.service_tier.is_none() {
            codex_request.service_tier = parsed_model
                .service_tier
                .clone()
                .or_else(|| self.model_config.service_tier.clone());
        }
        codex_request.service_tier = codex_request
            .service_tier
            .map(normalize_service_tier_for_upstream);
        let include_reasoning = codex_request.reasoning.is_some();

        let Some(mut account) = self.upstream.acquire_account(&codex_request.model).await else {
            return no_available_accounts_response().into_response();
        };
        let mut log_context = CodexRequestLogContext::new(
            request_id,
            &account.id,
            &codex_request.model,
            client_stream,
            started_at,
        );

        let mut excluded_account_ids = Vec::new();
        let response = loop {
            let response = self
                .upstream
                .send_codex_request_with_refresh_retry(&codex_request, &account, request_id)
                .await;
            self.upstream.release_account(&account.id).await;

            match response {
                Ok(response) => break response,
                Err(error) => {
                    if let Some(retry) = classify_upstream_account_retry(&error) {
                        let fallback = self
                            .upstream
                            .apply_retry_and_acquire_fallback(
                                &account,
                                retry,
                                &codex_request.model,
                                &mut excluded_account_ids,
                            )
                            .await;
                        self.upstream
                            .log_response(
                                &log_context,
                                retry.status(),
                                EventLevel::Warn,
                                "v1 chat completions upstream retrying with fallback account",
                                retry.metadata(client_stream),
                            )
                            .await;
                        if let Some(fallback) = fallback {
                            account = fallback;
                            log_context = log_context.with_account(&account.id);
                            continue;
                        }
                    }
                    let error_response = codex_client_error_response(error);
                    self.upstream
                        .log_response(
                            &log_context,
                            error_response.0,
                            EventLevel::Error,
                            "v1 chat completions upstream request failed",
                            json!({"stream": client_stream}),
                        )
                        .await;
                    return error_response.into_response();
                }
            }
        };
        if self
            .upstream
            .persist_cookies(&account.id, &response.set_cookie_headers)
            .await
            .is_err()
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to persist upstream cookies",
                    "cookie_store_error",
                )),
            )
                .into_response();
        }
        if let Some(usage) = response.usage {
            if self
                .upstream
                .record_usage(&account.id, usage)
                .await
                .is_err()
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(openai_error(
                        "Failed to record account usage",
                        "usage_store_error",
                    )),
                )
                    .into_response();
            }
        }

        if client_stream {
            match chat_completion_stream_from_codex_sse(
                &response.body,
                &display_model,
                include_reasoning,
            ) {
                Ok(Some(body)) => Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/event-stream")
                    .header(CACHE_CONTROL, "no-cache")
                    .body(Body::from(body))
                    .unwrap_or_else(|_| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(openai_error(
                                "Failed to build stream response",
                                "stream_response_error",
                            )),
                        )
                            .into_response()
                    }),
                Ok(None) => (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        "Codex response did not include response.completed",
                        "empty_upstream_response",
                    )),
                )
                    .into_response(),
                Err(_) => (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        "Invalid Codex SSE response",
                        "invalid_upstream_sse",
                    )),
                )
                    .into_response(),
            }
        } else {
            match chat_completion_from_codex_sse(&response.body, &display_model, include_reasoning)
            {
                Ok(Some(body)) => (StatusCode::OK, Json(body)).into_response(),
                Ok(None) => (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        "Codex response did not include response.completed",
                        "empty_upstream_response",
                    )),
                )
                    .into_response(),
                Err(_) => (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        "Invalid Codex SSE response",
                        "invalid_upstream_sse",
                    )),
                )
                    .into_response(),
            }
        }
    }
}
