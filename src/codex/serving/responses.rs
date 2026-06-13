use std::time::Instant;

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{
    codex::accounts::models::service::ModelService,
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::protocol::codex_to_openai::openai_error,
    codex::gateway::transport::types::CodexResponsesRequest,
    codex::logs::event::EventLevel,
    codex::serving::dispatch::{
        classify_upstream_account_retry, completed_response_json, no_available_accounts_response,
        normalize_service_tier_for_upstream, websocket_history_retry_metadata,
        CodexRequestLogContext, CodexUpstreamService, CollectedResponse,
    },
    codex::serving::http::errors::codex_client_error_response,
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

    pub async fn handle(
        &self,
        request_id: &str,
        headers: HeaderMap,
        body: Bytes,
        started_at: Instant,
    ) -> Response {
        let default_model = self.model_config.default_model.clone();
        let body = serde_json::from_slice::<ResponsesBody>(&body)
            .unwrap_or_else(|_| default_body(default_model.clone()));
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
                // 显式 false 是客户端兼容开关；previous_response_id 仍会在 transport 层强制 WebSocket。
                codex_request.force_http_sse = true;
            }
            None => {}
        }
        self.upstream
            .prepare_response_session(&mut codex_request)
            .await;
        let Some(mut account) = self
            .upstream
            .acquire_account_for_request(&codex_request)
            .await
        else {
            return no_available_accounts_response().into_response();
        };
        let mut log_context = CodexRequestLogContext::new(
            request_id,
            &account.id,
            &codex_request.model,
            client_stream,
            started_at,
        );

        if client_stream {
            return self
                .upstream
                .responses_stream(codex_request, account, log_context)
                .await;
        }

        let mut excluded_account_ids = Vec::new();
        let mut empty_response_retries = 0;
        const MAX_EMPTY_RETRIES: u8 = 2;

        let response = loop {
            let response = self
                .upstream
                .send_codex_request_with_refresh_retry(&codex_request, &account, request_id)
                .await;
            self.upstream.release_account(&account.id).await;

            match response {
                Ok(response) => {
                    // 检测空响应
                    if let Ok(CollectedResponse::Empty) = completed_response_json(&response.body) {
                        empty_response_retries += 1;
                        if empty_response_retries <= MAX_EMPTY_RETRIES {
                            self.upstream
                                .log_response(
                                    &log_context,
                                    StatusCode::OK,
                                    EventLevel::Warn,
                                    "v1 responses empty response, retrying",
                                    json!({
                                        "stream": false,
                                        "emptyResponse": true,
                                        "retryAttempt": empty_response_retries,
                                    }),
                                )
                                .await;
                            continue;
                        } else {
                            self.upstream
                                .log_response(
                                    &log_context,
                                    StatusCode::BAD_GATEWAY,
                                    EventLevel::Error,
                                    "v1 responses empty response after max retries",
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
                    }
                    break response;
                }
                Err(error) => {
                    if let Some(retry) = classify_upstream_account_retry(&error) {
                        if codex_request.previous_response_id.is_some() {
                            // previous_response_id history is account-affine upstream.
                            self.upstream.apply_account_retry(&account, retry).await;
                            self.upstream
                                .log_response(
                                &log_context,
                                retry.status(),
                                EventLevel::Warn,
                                "v1 responses websocket history request kept on original account",
                                websocket_history_retry_metadata(retry, false),
                            )
                            .await;
                        } else {
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
                                    "v1 responses upstream retrying with fallback account",
                                    retry.metadata(false),
                                )
                                .await;
                            if let Some(fallback) = fallback {
                                account = fallback;
                                log_context = log_context.with_account(&account.id);
                                continue;
                            }
                        }
                    }
                    let error_response = codex_client_error_response(error);
                    self.upstream
                        .log_response(
                            &log_context,
                            error_response.0,
                            EventLevel::Error,
                            "v1 responses upstream request failed",
                            json!({"stream": false}),
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
            self.upstream
                .log_response(
                    &log_context,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    EventLevel::Error,
                    "v1 responses cookie persistence failed",
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
                .record_usage(&account.id, usage)
                .await
                .is_err()
            {
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        EventLevel::Error,
                        "v1 responses usage persistence failed",
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

        match completed_response_json(&response.body) {
            Ok(CollectedResponse::Completed(body)) => {
                self.upstream
                    .record_response_affinity(
                        &codex_request,
                        &account.id,
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
                        "v1 responses completed",
                        json!({"stream": false, "usage": response.usage}),
                    )
                    .await;
                (StatusCode::OK, Json(body)).into_response()
            }
            Ok(CollectedResponse::Empty) => {
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::BAD_GATEWAY,
                        EventLevel::Error,
                        "v1 responses empty response",
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
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::BAD_GATEWAY,
                        EventLevel::Error,
                        "v1 responses upstream SSE failed",
                        failure.metadata(false),
                    )
                    .await;
                (
                    StatusCode::BAD_GATEWAY,
                    Json(openai_error(
                        &failure.openai_error_message(),
                        "upstream_error",
                    )),
                )
                    .into_response()
            }
            Ok(CollectedResponse::MissingCompleted) => {
                self.upstream
                    .log_response(
                        &log_context,
                        StatusCode::BAD_GATEWAY,
                        EventLevel::Warn,
                        "v1 responses completed event missing",
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
                        "v1 responses invalid SSE response",
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
}

#[derive(Deserialize)]
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
    prompt_cache_key: Option<String>,
    include: Option<Vec<String>>,
    client_metadata: Option<Value>,
    previous_response_id: Option<String>,
    stream: Option<bool>,
    #[serde(rename = "turnState")]
    turn_state: Option<String>,
    #[serde(rename = "turnMetadata")]
    turn_metadata: Option<String>,
    #[serde(rename = "betaFeatures")]
    beta_features: Option<String>,
    #[serde(rename = "includeTimingMetrics")]
    include_timing_metrics: Option<String>,
    #[serde(rename = "codexWindowId")]
    codex_window_id: Option<String>,
    #[serde(rename = "parentThreadId")]
    parent_thread_id: Option<String>,
    #[serde(alias = "useWebSocket")]
    use_websocket: Option<bool>,
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
    let include = request.include.get_or_insert_with(Vec::new);
    if include
        .iter()
        .any(|item| item == "reasoning.encrypted_content")
    {
        return;
    }
    include.push("reasoning.encrypted_content".to_string());
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
    let mut metadata = match client_metadata {
        Some(Value::Object(metadata)) => metadata,
        _ => Map::new(),
    };
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
