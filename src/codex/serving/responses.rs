use std::time::Instant;

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};

use crate::{
    codex::events::event::EventLevel,
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::protocol::codex_to_openai::openai_error,
    codex::gateway::protocol::tuple_schema::prepare_schema,
    codex::gateway::transport::types::{CodexCompactRequest, CodexResponsesRequest},
    codex::models::service::ModelService,
    codex::serving::dispatch::{
        affinity::SessionAffinityRepositoryResult, classify_upstream_account_retry,
        classify_upstream_request_recovery, completed_response_json,
        no_available_accounts_response, normalize_service_tier_for_upstream,
        websocket_history_retry_metadata, CodexRequestLogContext, CodexUpstreamService,
        CollectedResponse, ImplicitResumeSnapshot, UpstreamRequestRecovery,
    },
    codex::serving::http::errors::{
        codex_client_error_message, codex_client_error_response,
        codex_client_error_response_with_message,
    },
    config::ModelConfig,
};

const OPENAI_SUBAGENT_HEADER: &str = "x-openai-subagent";

#[derive(Clone)]
pub struct ResponsesService {
    model_config: ModelConfig,
    models: ModelService,
    upstream: CodexUpstreamService,
}

impl ResponsesService {
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

    /// 获取上游使用的指纹（用于诊断）
    pub fn upstream_fingerprint(&self) -> &Fingerprint {
        self.upstream.fingerprint()
    }

    pub async fn reload_session_affinity_from_repository(
        &self,
    ) -> SessionAffinityRepositoryResult<usize> {
        self.upstream
            .reload_session_affinity_from_repository()
            .await
    }

    pub async fn handle(
        &self,
        request_id: &str,
        headers: HeaderMap,
        body: Bytes,
        started_at: Instant,
    ) -> Response {
        let default_model = self.model_config.default_model.clone();
        let body = match parse_responses_body(&body, default_model.clone()) {
            Some(body) => body,
            None => return invalid_responses_body_response(),
        };
        let client_stream = body.stream.unwrap_or(true);
        let requested_model = body.model.clone().unwrap_or(default_model);
        let catalog = self.models.catalog().await;
        if !catalog.is_recognized_model_name(&requested_model) {
            return (
                StatusCode::NOT_FOUND,
                Json(openai_error("Model not found", "model_not_found")),
            )
                .into_response();
        }
        let parsed_model = catalog.parse_model_name(&requested_model);
        let mut codex_request = CodexResponsesRequest::new_http_sse(
            parsed_model.model_id.clone(),
            body.instructions.unwrap_or_default(),
            body.input.unwrap_or_default(),
        );
        codex_request.reasoning = body.reasoning;
        codex_request.tools = body.tools;
        codex_request.tool_choice = body.tool_choice;
        codex_request.parallel_tool_calls = body.parallel_tool_calls;
        codex_request.text = body.text;
        codex_request.tuple_schema = body.tuple_schema;
        codex_request.explicit_prompt_cache_key = body
            .prompt_cache_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        codex_request.prompt_cache_key = body.prompt_cache_key;
        codex_request.include = body.include;
        let openai_subagent =
            normalize_openai_subagent(header_string(&headers, OPENAI_SUBAGENT_HEADER).as_deref())
                .or_else(|| openai_subagent_from_metadata(body.client_metadata.as_ref()));
        codex_request.client_metadata =
            client_metadata_with_openai_subagent(body.client_metadata, openai_subagent);
        codex_request.previous_response_id = body.previous_response_id;
        codex_request.reasoning = responses_reasoning(
            codex_request.reasoning.take(),
            parsed_model.reasoning_effort.as_deref(),
            self.model_config.default_reasoning_effort.as_deref(),
        );
        codex_request.service_tier = body
            .service_tier
            .or(parsed_model.service_tier)
            .or_else(|| self.model_config.service_tier.clone())
            .map(normalize_service_tier_for_upstream);
        ensure_reasoning_include(&mut codex_request);
        codex_request.turn_state = body
            .turn_state
            .or_else(|| header_string(&headers, "x-codex-turn-state"));
        codex_request.turn_metadata = body
            .turn_metadata
            .or_else(|| header_string(&headers, "x-codex-turn-metadata"));
        codex_request.beta_features = body
            .beta_features
            .or_else(|| header_string(&headers, "x-codex-beta-features"));
        codex_request.include_timing_metrics = body
            .include_timing_metrics
            .or_else(|| header_string(&headers, "x-responsesapi-include-timing-metrics"));
        codex_request.version = header_string(&headers, "version");
        codex_request.codex_window_id = body
            .codex_window_id
            .or_else(|| header_string(&headers, "x-codex-window-id"));
        codex_request.parent_thread_id = body
            .parent_thread_id
            .or_else(|| header_string(&headers, "x-codex-parent-thread-id"));
        match body.use_websocket {
            Some(true) => codex_request.use_websocket = true,
            Some(false) => {
                // 显式传输控制不覆盖历史续链；previous_response_id 会在 transport 层强制 WebSocket。
                codex_request.force_http_sse = true;
            }
            None => {}
        }
        let mut implicit_resume = self
            .upstream
            .prepare_response_session(&mut codex_request)
            .await;
        let Some(mut acquired) = self
            .upstream
            .acquire_account_for_request(&codex_request)
            .await
        else {
            return no_available_accounts_response().into_response();
        };
        let mut log_context = CodexRequestLogContext::new(
            request_id,
            &acquired.account.id,
            &codex_request.model,
            client_stream,
            started_at,
        );

        if client_stream {
            return self
                .upstream
                .responses_stream(codex_request, acquired, log_context, implicit_resume)
                .await;
        }

        let mut excluded_account_ids = Vec::new();
        let mut empty_response_retries = 0;
        let mut history_recovery_used = false;
        let mut model_unsupported_retry_used = false;
        const MAX_EMPTY_RETRIES: u8 = 2;

        let (response, collected_response) = loop {
            self.upstream
                .stagger_request(acquired.previous_slot_at)
                .await;
            let response = self
                .upstream
                .send_codex_request_with_upstream_retries(
                    &codex_request,
                    &acquired.account,
                    request_id,
                )
                .await;
            self.upstream.release_account(&acquired.account.id).await;

            match response {
                Ok(response) => {
                    let collected_response = completed_response_json(
                        &response.body,
                        codex_request.tuple_schema.as_ref(),
                    );
                    match &collected_response {
                        Ok(CollectedResponse::Empty) => {
                            if self
                                .upstream
                                .record_empty_response(&acquired.account.id)
                                .await
                                .is_err()
                            {
                                self.upstream
                                    .log_response(
                                        &log_context,
                                        StatusCode::OK,
                                        EventLevel::Warn,
                                        "v1 responses 记录空响应计数失败",
                                        json!({
                                            "stream": false,
                                            "emptyResponse": true,
                                            "emptyResponseStoreError": true,
                                        }),
                                    )
                                    .await;
                            }
                            empty_response_retries += 1;
                            if empty_response_retries <= MAX_EMPTY_RETRIES {
                                self.upstream
                                    .log_response(
                                        &log_context,
                                        StatusCode::OK,
                                        EventLevel::Warn,
                                        "v1 responses 空响应，准备重试",
                                        json!({
                                            "stream": false,
                                            "emptyResponse": true,
                                            "retryAttempt": empty_response_retries,
                                        }),
                                    )
                                    .await;
                                continue;
                            }
                            self.upstream
                                .log_response(
                                    &log_context,
                                    StatusCode::BAD_GATEWAY,
                                    EventLevel::Error,
                                    "v1 responses 空响应重试次数已耗尽",
                                    json!({
                                        "stream": false,
                                        "emptyResponse": true,
                                        "retriesExhausted": true,
                                    }),
                                )
                                .await;
                            return (
                                StatusCode::BAD_GATEWAY,
                                Json(openai_error(
                                    "Codex response was empty after retries",
                                    "empty_upstream_response",
                                )),
                            )
                                .into_response();
                        }
                        Ok(CollectedResponse::Failed(failure)) => {
                            if failure.invalid_reasoning_replay() {
                                self.upstream
                                    .evict_reasoning_replay(&codex_request, &acquired.account.id)
                                    .await;
                            }
                            let error = failure.upstream_error();
                            if let Some(recovery) =
                                classify_upstream_request_recovery(&error, history_recovery_used)
                            {
                                self.apply_request_recovery(
                                    &mut codex_request,
                                    &mut history_recovery_used,
                                    &mut implicit_resume,
                                    &log_context,
                                    recovery,
                                )
                                .await;
                                continue;
                            }
                            if let Some(retry) = classify_upstream_account_retry(
                                &error,
                                model_unsupported_retry_used,
                            ) {
                                if retry.is_model_unsupported() {
                                    model_unsupported_retry_used = true;
                                }
                                if codex_request.previous_response_id.is_some()
                                    && retry.preserve_history_account_affinity()
                                {
                                    self.upstream
                                        .apply_account_retry(&acquired.account, retry)
                                        .await;
                                    self.upstream
                                        .log_response(
                                            &log_context,
                                            retry.status(),
                                            EventLevel::Warn,
                                            "v1 responses SSE history 失败保持原账户",
                                            websocket_history_retry_metadata(retry, false),
                                        )
                                        .await;
                                } else {
                                    let fallback = self
                                        .upstream
                                        .apply_retry_and_acquire_fallback(
                                            &acquired.account,
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
                                            "v1 responses 上游 SSE 失败将使用备用账户重试",
                                            retry.metadata(false),
                                        )
                                        .await;
                                    if let Some(fallback) = fallback {
                                        log_context =
                                            log_context.with_account(&fallback.account.id);
                                        acquired = fallback;
                                        continue;
                                    }
                                    let message = self
                                        .upstream
                                        .fallback_exhausted_message(&codex_client_error_message(
                                            &error,
                                        ))
                                        .await;
                                    let error_response =
                                        codex_client_error_response_with_message(error, &message);
                                    self.upstream
                                        .log_response(
                                            &log_context,
                                            error_response.0,
                                            EventLevel::Error,
                                            "v1 responses 上游 SSE fallback 已耗尽",
                                            failure.metadata(false),
                                        )
                                        .await;
                                    return error_response.into_response();
                                }
                            }
                            let error_response = codex_client_error_response(error);
                            self.upstream
                                .log_response(
                                    &log_context,
                                    error_response.0,
                                    EventLevel::Error,
                                    "v1 responses 上游 SSE 失败",
                                    failure.metadata(false),
                                )
                                .await;
                            return error_response.into_response();
                        }
                        _ => {}
                    }
                    break (response, collected_response);
                }
                Err(error) => {
                    if let Some(recovery) =
                        classify_upstream_request_recovery(&error, history_recovery_used)
                    {
                        self.apply_request_recovery(
                            &mut codex_request,
                            &mut history_recovery_used,
                            &mut implicit_resume,
                            &log_context,
                            recovery,
                        )
                        .await;
                        continue;
                    }
                    if let Some(retry) =
                        classify_upstream_account_retry(&error, model_unsupported_retry_used)
                    {
                        if retry.is_model_unsupported() {
                            model_unsupported_retry_used = true;
                        }
                        if codex_request.previous_response_id.is_some()
                            && retry.preserve_history_account_affinity()
                        {
                            // previous_response_id history is account-affine upstream.
                            self.upstream
                                .apply_account_retry(&acquired.account, retry)
                                .await;
                            self.upstream
                                .log_response(
                                    &log_context,
                                    retry.status(),
                                    EventLevel::Warn,
                                    "v1 responses WebSocket history 请求保持原账户",
                                    websocket_history_retry_metadata(retry, false),
                                )
                                .await;
                        } else {
                            let fallback = self
                                .upstream
                                .apply_retry_and_acquire_fallback(
                                    &acquired.account,
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
                                    "v1 responses 上游请求将使用备用账户重试",
                                    retry.metadata(false),
                                )
                                .await;
                            if let Some(fallback) = fallback {
                                log_context = log_context.with_account(&fallback.account.id);
                                acquired = fallback;
                                continue;
                            }
                            let message = self
                                .upstream
                                .fallback_exhausted_message(&codex_client_error_message(&error))
                                .await;
                            let error_response =
                                codex_client_error_response_with_message(error, &message);
                            self.upstream
                                .log_response(
                                    &log_context,
                                    error_response.0,
                                    EventLevel::Error,
                                    "v1 responses 上游请求 fallback 已耗尽",
                                    json!({"stream": false}),
                                )
                                .await;
                            return error_response.into_response();
                        }
                    }
                    let error_response = codex_client_error_response(error);
                    self.upstream
                        .log_response(
                            &log_context,
                            error_response.0,
                            EventLevel::Error,
                            "v1 responses 上游请求失败",
                            json!({"stream": false}),
                        )
                        .await;
                    return error_response.into_response();
                }
            }
        };
        if self
            .upstream
            .persist_cookies(&acquired.account.id, &response.set_cookie_headers)
            .await
            .is_err()
        {
            self.upstream
                .log_response(
                    &log_context,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    EventLevel::Error,
                    "v1 responses 持久化 cookie 失败",
                    json!({"stream": false, "cookieStoreError": true}),
                )
                .await;
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
                .record_usage(&acquired.account.id, usage)
                .await
                .is_err()
            {
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        EventLevel::Error,
                        "v1 responses 持久化 usage 失败",
                        json!({"stream": false, "usage": usage, "usageStoreError": true}),
                    )
                    .await;
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

        match collected_response {
            Ok(CollectedResponse::Completed(body)) => {
                self.upstream
                    .record_response_affinity(
                        &codex_request,
                        &acquired.account.id,
                        &response.body,
                        response.turn_state.as_deref(),
                        response.usage,
                    )
                    .await;
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::OK,
                        EventLevel::Info,
                        "v1 responses 已完成",
                        json!({"stream": false, "usage": response.usage}),
                    )
                    .await;
                (StatusCode::OK, Json(body)).into_response()
            }
            Ok(CollectedResponse::Empty) => {
                if self
                    .upstream
                    .record_empty_response(&acquired.account.id)
                    .await
                    .is_err()
                {
                    self.upstream
                        .log_response(
                            &log_context,
                            StatusCode::OK,
                            EventLevel::Warn,
                            "v1 responses 记录空响应计数失败",
                            json!({
                                "stream": false,
                                "emptyResponse": true,
                                "emptyResponseStoreError": true,
                            }),
                        )
                        .await;
                }
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::BAD_GATEWAY,
                        EventLevel::Error,
                        "v1 responses 空响应",
                        json!({"stream": false, "emptyResponse": true}),
                    )
                    .await;
                (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        "Codex response was empty",
                        "empty_upstream_response",
                    )),
                )
                    .into_response()
            }
            Ok(CollectedResponse::Failed(failure)) => {
                if failure.invalid_reasoning_replay() {
                    self.upstream
                        .evict_reasoning_replay(&codex_request, &acquired.account.id)
                        .await;
                }
                let error_response = codex_client_error_response(failure.upstream_error());
                self.upstream
                    .log_response(
                        &log_context,
                        error_response.0,
                        EventLevel::Error,
                        "v1 responses 上游 SSE 失败",
                        failure.metadata(false),
                    )
                    .await;
                error_response.into_response()
            }
            Ok(CollectedResponse::MissingCompleted) => {
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::BAD_GATEWAY,
                        EventLevel::Warn,
                        "v1 responses 缺少 completed 事件",
                        json!({"stream": false, "usage": response.usage}),
                    )
                    .await;
                (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        "Codex response did not include response.completed",
                        "empty_upstream_response",
                    )),
                )
                    .into_response()
            }
            Err(error) => {
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::BAD_GATEWAY,
                        EventLevel::Warn,
                        "v1 responses SSE 响应无效",
                        json!({"stream": false, "sseParseError": error.to_string()}),
                    )
                    .await;
                (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        "Invalid Codex SSE response",
                        "invalid_upstream_sse",
                    )),
                )
                    .into_response()
            }
        }
    }

    pub async fn handle_compact(
        &self,
        request_id: &str,
        body: Bytes,
        started_at: Instant,
    ) -> Response {
        let default_model = self.model_config.default_model.clone();
        let body = match parse_compact_body(&body, default_model) {
            Some(body) => body,
            None => return invalid_responses_body_response(),
        };
        let catalog = self.models.catalog().await;
        if !catalog.is_recognized_model_name(&body.model) {
            return (
                StatusCode::NOT_FOUND,
                Json(openai_error("Model not found", "model_not_found")),
            )
                .into_response();
        }
        let parsed_model = catalog.parse_model_name(&body.model);
        let compact_request = CodexCompactRequest {
            model: parsed_model.model_id,
            input: body.input,
            instructions: body.instructions,
            tools: body.tools,
            parallel_tool_calls: body.parallel_tool_calls,
            reasoning: body.reasoning,
            text: body.text,
        };

        let Some(mut acquired) = self.upstream.acquire_account(&compact_request.model).await else {
            return no_available_accounts_response().into_response();
        };
        let mut log_context = CodexRequestLogContext::new(
            request_id,
            &acquired.account.id,
            &compact_request.model,
            false,
            started_at,
        );
        let mut excluded_account_ids = Vec::new();
        let mut model_unsupported_retry_used = false;
        let mut attempts = 0_u8;
        const MAX_COMPACT_RETRIES: u8 = 8;

        loop {
            attempts += 1;
            self.upstream
                .stagger_request(acquired.previous_slot_at)
                .await;
            let response = self
                .upstream
                .send_compact_request_with_upstream_retries(
                    &compact_request,
                    &acquired.account,
                    request_id,
                )
                .await;
            self.upstream.release_account(&acquired.account.id).await;

            match response {
                Ok(response) => {
                    if self
                        .upstream
                        .persist_cookies(&acquired.account.id, &response.set_cookie_headers)
                        .await
                        .is_err()
                    {
                        self.upstream
                            .log_response(
                                &log_context,
                                StatusCode::INTERNAL_SERVER_ERROR,
                                EventLevel::Error,
                                "v1 responses compact 持久化 cookie 失败",
                                json!({"stream": false, "compact": true, "cookieStoreError": true}),
                            )
                            .await;
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(openai_error(
                                "Failed to persist upstream cookies",
                                "cookie_store_error",
                            )),
                        )
                            .into_response();
                    }
                    self.upstream
                        .log_response(
                            &log_context,
                            StatusCode::OK,
                            EventLevel::Info,
                            "v1 responses compact 已完成",
                            json!({"stream": false, "compact": true}),
                        )
                        .await;
                    return (StatusCode::OK, Json(response.body)).into_response();
                }
                Err(error) => {
                    if attempts < MAX_COMPACT_RETRIES {
                        if let Some(retry) =
                            classify_upstream_account_retry(&error, model_unsupported_retry_used)
                        {
                            if retry.is_model_unsupported() {
                                model_unsupported_retry_used = true;
                            }
                            let fallback = self
                                .upstream
                                .apply_retry_and_acquire_fallback(
                                    &acquired.account,
                                    retry,
                                    &compact_request.model,
                                    &mut excluded_account_ids,
                                )
                                .await;
                            self.upstream
                                .log_response(
                                    &log_context,
                                    retry.status(),
                                    EventLevel::Warn,
                                    "v1 responses compact 上游请求将使用备用账户重试",
                                    retry.metadata(false),
                                )
                                .await;
                            if let Some(fallback) = fallback {
                                log_context = log_context.with_account(&fallback.account.id);
                                acquired = fallback;
                                continue;
                            }
                            let message = self
                                .upstream
                                .fallback_exhausted_message(&codex_client_error_message(&error))
                                .await;
                            let error_response =
                                codex_client_error_response_with_message(error, &message);
                            self.upstream
                                .log_response(
                                    &log_context,
                                    error_response.0,
                                    EventLevel::Error,
                                    "v1 responses compact fallback 已耗尽",
                                    json!({"stream": false, "compact": true}),
                                )
                                .await;
                            return error_response.into_response();
                        }
                    }
                    let error_response = codex_client_error_response(error);
                    self.upstream
                        .log_response(
                            &log_context,
                            error_response.0,
                            EventLevel::Error,
                            "v1 responses compact 上游请求失败",
                            json!({"stream": false, "compact": true}),
                        )
                        .await;
                    return error_response.into_response();
                }
            }
        }
    }

    async fn apply_request_recovery(
        &self,
        request: &mut CodexResponsesRequest,
        history_recovery_used: &mut bool,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
        log_context: &CodexRequestLogContext,
        recovery: UpstreamRequestRecovery,
    ) {
        *history_recovery_used = true;
        let stale_response_id = request.previous_response_id.clone();
        if let Some(response_id) = stale_response_id.as_deref() {
            self.upstream.forget_response_affinity(response_id).await;
        }
        if let Some(snapshot) = implicit_resume.take() {
            snapshot.restore(request);
        }
        request.previous_response_id = None;
        request.turn_state = None;
        self.upstream
            .log_response(
                log_context,
                StatusCode::BAD_REQUEST,
                EventLevel::Warn,
                "v1 responses 上游历史失效，去除 previous_response_id 后重试",
                recovery.metadata(false, stale_response_id.as_deref()),
            )
            .await;
    }
}

struct ResponsesBody {
    model: Option<String>,
    input: Option<Vec<Value>>,
    instructions: Option<String>,
    reasoning: Option<Value>,
    tools: Option<Vec<Value>>,
    service_tier: Option<String>,
    tool_choice: Option<Value>,
    parallel_tool_calls: Option<bool>,
    text: Option<Value>,
    tuple_schema: Option<Value>,
    prompt_cache_key: Option<String>,
    include: Option<Vec<String>>,
    client_metadata: Option<Value>,
    previous_response_id: Option<String>,
    stream: Option<bool>,
    turn_state: Option<String>,
    turn_metadata: Option<String>,
    beta_features: Option<String>,
    include_timing_metrics: Option<String>,
    codex_window_id: Option<String>,
    parent_thread_id: Option<String>,
    use_websocket: Option<bool>,
}

struct CompactBody {
    model: String,
    input: Vec<Value>,
    instructions: String,
    reasoning: Option<Value>,
    tools: Option<Vec<Value>>,
    parallel_tool_calls: Option<bool>,
    text: Option<Value>,
}

fn parse_responses_body(body: &[u8], default_model: String) -> Option<ResponsesBody> {
    let Value::Object(mut body) = serde_json::from_slice::<Value>(body).ok()? else {
        return None;
    };

    let mut parsed = default_body(default_model);
    parsed.model = take_string(&mut body, "model").or(parsed.model);
    parsed.input = take_array(&mut body, "input").map(sanitize_codex_input_items);
    parsed.instructions = take_string(&mut body, "instructions").or(parsed.instructions);
    parsed.reasoning = take_object_value(&mut body, "reasoning");
    parsed.tools = take_non_empty_array(&mut body, "tools");
    parsed.service_tier = take_string(&mut body, "service_tier");
    parsed.tool_choice = body.remove("tool_choice");
    parsed.parallel_tool_calls = take_bool(&mut body, "parallel_tool_calls");
    let prepared_text = take_prepared_text_format(&mut body);
    parsed.text = prepared_text.as_ref().map(|text| text.text.clone());
    parsed.tuple_schema = prepared_text.and_then(|text| text.tuple_schema);
    parsed.prompt_cache_key = take_string(&mut body, "prompt_cache_key");
    parsed.include = take_string_array(&mut body, "include");
    parsed.client_metadata = sanitize_client_metadata(body.remove("client_metadata"));
    parsed.previous_response_id = take_string(&mut body, "previous_response_id");
    parsed.stream = take_bool(&mut body, "stream");
    parsed.turn_state = take_non_empty_string(&mut body, "turnState");
    parsed.turn_metadata = take_non_empty_string(&mut body, "turnMetadata");
    parsed.beta_features = take_non_empty_string(&mut body, "betaFeatures");
    parsed.include_timing_metrics = take_non_empty_string(&mut body, "includeTimingMetrics");
    parsed.codex_window_id = take_non_empty_string(&mut body, "codexWindowId");
    parsed.parent_thread_id = take_non_empty_string(&mut body, "parentThreadId");
    parsed.use_websocket = take_bool(&mut body, "use_websocket");
    Some(parsed)
}

fn parse_compact_body(body: &[u8], default_model: String) -> Option<CompactBody> {
    let Value::Object(mut body) = serde_json::from_slice::<Value>(body).ok()? else {
        return None;
    };

    Some(CompactBody {
        model: take_string(&mut body, "model").unwrap_or(default_model),
        input: take_array(&mut body, "input")
            .map(sanitize_codex_input_items)
            .unwrap_or_default(),
        instructions: take_string(&mut body, "instructions").unwrap_or_default(),
        reasoning: sanitize_compact_reasoning(take_object_value(&mut body, "reasoning")),
        tools: take_non_empty_array(&mut body, "tools"),
        parallel_tool_calls: take_bool(&mut body, "parallel_tool_calls"),
        text: take_text_format(&mut body),
    })
}

fn invalid_responses_body_response() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(openai_error(
            "Request body must be a JSON object",
            "invalid_request",
        )),
    )
        .into_response()
}

fn default_body(default_model: String) -> ResponsesBody {
    ResponsesBody {
        model: Some(default_model),
        input: Some(Vec::new()),
        instructions: Some(String::new()),
        reasoning: None,
        tools: None,
        service_tier: None,
        tool_choice: None,
        parallel_tool_calls: None,
        text: None,
        tuple_schema: None,
        prompt_cache_key: None,
        include: None,
        client_metadata: None,
        previous_response_id: None,
        stream: None,
        turn_state: None,
        turn_metadata: None,
        beta_features: None,
        include_timing_metrics: None,
        codex_window_id: None,
        parent_thread_id: None,
        use_websocket: None,
    }
}

fn take_string(body: &mut Map<String, Value>, key: &str) -> Option<String> {
    body.remove(key).and_then(|value| match value {
        Value::String(value) => Some(value),
        _ => None,
    })
}

fn take_non_empty_string(body: &mut Map<String, Value>, key: &str) -> Option<String> {
    take_string(body, key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn take_bool(body: &mut Map<String, Value>, key: &str) -> Option<bool> {
    body.remove(key).and_then(|value| value.as_bool())
}

fn take_array(body: &mut Map<String, Value>, key: &str) -> Option<Vec<Value>> {
    body.remove(key).and_then(|value| match value {
        Value::Array(values) => Some(values),
        _ => None,
    })
}

fn take_non_empty_array(body: &mut Map<String, Value>, key: &str) -> Option<Vec<Value>> {
    take_array(body, key).filter(|values| !values.is_empty())
}

fn take_object_value(body: &mut Map<String, Value>, key: &str) -> Option<Value> {
    body.remove(key).and_then(|value| match value {
        Value::Object(_) => Some(value),
        _ => None,
    })
}

fn take_string_array(body: &mut Map<String, Value>, key: &str) -> Option<Vec<String>> {
    let values = take_array(body, key)?;
    values
        .into_iter()
        .map(|value| match value {
            Value::String(value) => Some(value),
            _ => None,
        })
        .collect()
}

struct PreparedTextFormat {
    text: Value,
    tuple_schema: Option<Value>,
}

fn take_prepared_text_format(body: &mut Map<String, Value>) -> Option<PreparedTextFormat> {
    take_text_format_inner(body, true)
}

fn take_text_format(body: &mut Map<String, Value>) -> Option<Value> {
    take_text_format_inner(body, false).map(|prepared| prepared.text)
}

fn take_text_format_inner(
    body: &mut Map<String, Value>,
    prepare_tuple_schema: bool,
) -> Option<PreparedTextFormat> {
    let Value::Object(text) = body.remove("text")? else {
        return None;
    };
    let Value::Object(format) = text.get("format")? else {
        return None;
    };
    let format_type = format.get("type")?.as_str()?;
    let mut sanitized_format = Map::new();
    sanitized_format.insert("type".to_string(), Value::String(format_type.to_string()));
    let mut tuple_schema = None;
    if let Some(name) = format.get("name").and_then(Value::as_str) {
        sanitized_format.insert("name".to_string(), Value::String(name.to_string()));
    }
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

    let mut sanitized_text = Map::new();
    sanitized_text.insert("format".to_string(), Value::Object(sanitized_format));
    Some(PreparedTextFormat {
        text: Value::Object(sanitized_text),
        tuple_schema,
    })
}

fn sanitize_client_metadata(client_metadata: Option<Value>) -> Option<Value> {
    let Value::Object(input) = client_metadata? else {
        return None;
    };
    let metadata: Map<String, Value> = input
        .into_iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key, Value::String(value.to_string())))
        })
        .collect();
    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

fn sanitize_compact_reasoning(reasoning: Option<Value>) -> Option<Value> {
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

fn sanitize_codex_input_items(input: Vec<Value>) -> Vec<Value> {
    input
        .into_iter()
        .filter_map(|item| {
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

fn sanitize_reasoning_item(item: &Map<String, Value>) -> Option<Value> {
    let id = non_empty_string(item.get("id"))?;
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
    if let Some(encrypted_content) = non_empty_string(item.get("encrypted_content")) {
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
    let summary: Vec<Value> = parts
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
        .collect();
    Some(summary)
}

fn sanitize_reasoning_content(value: Option<&Value>) -> Option<Vec<Value>> {
    let Value::Array(parts) = value? else {
        return None;
    };
    let content: Vec<Value> = parts
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
        .collect();
    (!content.is_empty()).then_some(content)
}

fn sanitize_compaction_item(item: &Map<String, Value>) -> Option<Value> {
    let encrypted_content = non_empty_string(item.get("encrypted_content"))?;
    let mut sanitized = Map::new();
    sanitized.insert("type".to_string(), Value::String("compaction".to_string()));
    sanitized.insert(
        "encrypted_content".to_string(),
        Value::String(encrypted_content.to_string()),
    );
    if let Some(id) = non_empty_string(item.get("id")) {
        sanitized.insert("id".to_string(), Value::String(id.to_string()));
    }
    Some(Value::Object(sanitized))
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    let value = value?.as_str()?;
    (!value.trim().is_empty()).then_some(value)
}

fn responses_reasoning(
    client_reasoning: Option<Value>,
    suffix_effort: Option<&str>,
    default_effort: Option<&str>,
) -> Option<Value> {
    let effort = client_reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .or(suffix_effort)
        .or(default_effort);
    let summary = client_reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.get("summary"))
        .and_then(Value::as_str)
        .unwrap_or("auto");
    if effort.is_none() && client_reasoning.is_none() {
        return None;
    }
    let mut reasoning = json!({"summary": summary});
    if let Some(effort) = effort {
        reasoning["effort"] = Value::String(effort.to_string());
    }
    Some(reasoning)
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

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

fn client_metadata_with_openai_subagent(
    client_metadata: Option<Value>,
    openai_subagent: Option<String>,
) -> Option<Value> {
    let mut metadata = sanitize_client_metadata(client_metadata)
        .and_then(|value| match value {
            Value::Object(metadata) => Some(metadata),
            _ => None,
        })
        .unwrap_or_default();
    metadata.remove(OPENAI_SUBAGENT_HEADER);
    if let Some(openai_subagent) = openai_subagent {
        metadata.insert(
            OPENAI_SUBAGENT_HEADER.to_string(),
            Value::String(openai_subagent),
        );
    }

    if metadata.is_empty() {
        None
    } else {
        Some(Value::Object(metadata))
    }
}

fn openai_subagent_from_metadata(client_metadata: Option<&Value>) -> Option<String> {
    normalize_openai_subagent(
        client_metadata?
            .as_object()?
            .get(OPENAI_SUBAGENT_HEADER)?
            .as_str(),
    )
}

fn normalize_openai_subagent(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if matches!(
        value,
        "review" | "compact" | "memory_consolidation" | "collab_spawn"
    ) {
        Some(value.to_string())
    } else {
        None
    }
}
