//! Responses 创建编排与调度服务。
//!
//! 包含了将 OpenAI 请求调度到 Codex 上游账号的完整逻辑，包括：
//! - 响应创建（非流式 / 流式）
//! - 会话亲和性与隐式续接
//! - reasoning replay
//! - 账号回退与错误恢复
//! - 配额验证

use std::{pin::Pin, sync::Arc, time::Instant};

use bytes::Bytes;
use chrono::Utc;
use futures::stream::Stream;
use serde_json::{Value, json};

use crate::{
    dispatch::{
        affinity::resolve::{prepare_variant_identity, record_response_affinity},
        affinity::{AccountIdentityService, SessionAffinityService},
        attempts::AccountAttemptLedger,
        errors::{
            backend_transport_name, is_continuation_busy_error, is_history_recovery_upstream_error,
            is_retryable_account_transport_error, upstream_error_body,
            upstream_error_set_cookie_headers,
        },
        recovery::{
            account_failure::{isolate_rotatable_account_failure, isolate_sse_account_failure},
            cloudflare::CloudflareRecovery,
            exhaustion::AccountExhaustionTracker,
            history::HistoryRecoveryPlan,
        },
        upstream_call::{
            AccountUpstreamContext, QUOTA_VERIFY_LIMIT_REACHED_MESSAGE, QuotaVerificationContext,
            QuotaVerificationDecision, create_response_with_account,
            verify_acquired_quota_if_required,
        },
    },
    fleet::{
        account::Account,
        pool::{AccountAcquireRequest, AccountPoolService},
    },
    models::service::ModelService,
    telemetry::{
        recorder::{
            Recorder, enrich_response_request_semantics, reasoning_effort_from_request,
            record_response_event,
        },
        usage::types::ResponseUsageRecord,
    },
    upstream::openai::{
        protocol::{
            events::TokenUsage,
            responses::{CodexResponsesRequest, CollectedResponse, response_from_codex_sse},
        },
        transport::{
            CodexBackendClient, CodexBackendResponse, backend_transport_for_response_request,
        },
    },
};

use super::{
    errors::{ResponseDispatchError, ResponseDispatchStreamError},
    recording::{
        ResponseDispatchErrorDetails, ResponseDispatchErrorEventRecord,
        ResponseUpstreamErrorEventRecord, insert_response_status_metadata,
        insert_response_trace_metadata, insert_response_upstream_diagnostics,
        insert_websocket_pool_decision, record_response_dispatch_error_event,
        record_response_upstream_error_event,
    },
    stream::{
        sse_failure::is_history_recovery_sse_failure,
        trace::{ResponseDispatchAttempt, ResponseDispatchTrace},
    },
};

/// OpenAI Responses 调度服务。
#[derive(Clone)]
pub struct ResponseDispatchService {
    pub(in crate::dispatch) account_pool: Arc<AccountPoolService>,
    pub(in crate::dispatch) models: Arc<ModelService>,
    pub(in crate::dispatch) codex: Arc<CodexBackendClient>,
    pub(in crate::dispatch) session_affinity: Arc<SessionAffinityService>,
    pub(in crate::dispatch) account_identity: Arc<AccountIdentityService>,
    pub(in crate::dispatch) recorder: Arc<Recorder>,
    pub(in crate::dispatch) cloudflare: CloudflareRecovery,
}

pub(crate) struct ResponseDispatchServiceParts {
    pub account_pool: Arc<AccountPoolService>,
    pub models: Arc<ModelService>,
    pub codex: Arc<CodexBackendClient>,
    pub session_affinity: Arc<SessionAffinityService>,
    pub account_identity: Arc<AccountIdentityService>,
    pub recorder: Arc<Recorder>,
    pub cloudflare: CloudflareRecovery,
}

/// Responses live SSE 响应体流。
pub type ResponseBodyStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, ResponseDispatchStreamError>> + Send + 'static>>;

/// Responses live SSE 调度结果。
pub struct ResponseDispatchStream {
    pub body: ResponseBodyStream,
    pub response_headers: Vec<(String, String)>,
}

/// Responses 非流式调度结果。
pub struct ResponseDispatchResponse {
    pub body: Value,
    pub response_headers: Vec<(String, String)>,
}

impl ResponseDispatchService {
    pub(crate) fn new(parts: ResponseDispatchServiceParts) -> Self {
        Self {
            account_pool: parts.account_pool,
            models: parts.models,
            codex: parts.codex,
            session_affinity: parts.session_affinity,
            account_identity: parts.account_identity,
            recorder: parts.recorder,
            cloudflare: parts.cloudflare,
        }
    }

    async fn record_response_affinity(
        &self,
        history: &HistoryRecoveryPlan,
        original_request: &CodexResponsesRequest,
        account_id: &str,
        body: &str,
        turn_state: Option<String>,
        usage: Option<TokenUsage>,
    ) {
        record_response_affinity(
            &self.session_affinity,
            history,
            original_request,
            account_id,
            body,
            turn_state,
            usage,
        )
        .await;
    }
}

impl ResponseDispatchService {
    /// 调度非流式 Responses 请求到 Codex Responses 上游。
    pub async fn complete(
        &self,
        request_id: &str,
        route: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<ResponseDispatchResponse, ResponseDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        let compact = request.semantics().compact;
        let tuple_schema = request.tuple_schema.clone();
        let image_generation_requested = request.expects_image_generation();
        let now = Utc::now();
        prepare_variant_identity(&mut request);
        self.account_identity.prepare_local_identity(&mut request);
        let mut history = HistoryRecoveryPlan::load(&self.session_affinity, &request).await;
        let preferred_account_id = self
            .preferred_account_id_for_request(&request, &history, now)
            .await;
        let mut acquire_request = AccountAcquireRequest::new(request.model(), now);
        if let Some(preferred_account_id) = preferred_account_id {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut candidates =
            AccountAttemptLedger::freeze(&self.account_pool, &acquire_request).await;
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut next_required_account_id: Option<String> = None;
        let mut trace = ResponseDispatchTrace::default();
        let (account, response, collected_response, attempt): (
            Account,
            CodexBackendResponse,
            CollectedResponse,
            ResponseDispatchAttempt,
        ) = loop {
            let acquired = if let Some(account_id) = next_required_account_id.take() {
                match self
                    .account_pool
                    .acquire_with(
                        &AccountAcquireRequest::new(request.model(), Utc::now())
                            .with_required_account_id(account_id),
                    )
                    .await
                {
                    Some(acquired) => Some(acquired),
                    None => candidates.acquire_next(&self.account_pool).await,
                }
            } else {
                candidates.acquire_next(&self.account_pool).await
            };
            let Some(acquired) = acquired else {
                tracing::info!(
                    candidate_count = candidates.candidate_count(),
                    attempted = candidates.attempted_count(),
                    state_excluded = candidates.state_excluded_count(),
                    "Responses account candidate ledger exhausted"
                );
                let error = exhausted_accounts
                    .last_exhausted()
                    .map(ResponseDispatchError::from_exhausted_account)
                    .unwrap_or(ResponseDispatchError::NoActiveAccount);
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: exhausted_accounts.last_account_id(),
                        stream: false,
                        compact,
                        transport: Some(backend_transport_name(
                            backend_transport_for_response_request(&request),
                        )),
                    },
                    &error,
                )
                .await;
                return Err(error);
            };
            let acquired_account_id = acquired.account.id.clone();
            // 配额验证
            let acquired = match verify_acquired_quota_if_required(
                QuotaVerificationContext {
                    account_pool: self.account_pool.as_ref(),
                    codex: self.codex.as_ref(),
                    cloudflare: &self.cloudflare,
                    account_identity: &self.account_identity,
                    request_id,
                },
                acquired,
            )
            .await
            {
                QuotaVerificationDecision::Ready(acquired) => *acquired,
                QuotaVerificationDecision::RetryWithAnotherAccount => {
                    exhausted_accounts.record_rate_limited(
                        Some(&acquired_account_id),
                        QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
                    );
                    continue;
                }
            };

            let Some(attempt_request) = history.prepare_attempt(&request, &acquired.account.id)
            else {
                acquired.release_without_usage().await;
                return Err(ResponseDispatchError::HistoryUnavailable {
                    upstream_error: "previous response history cannot be sent to another account"
                        .to_string(),
                });
            };
            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account.clone();
            let release_account_id = account.id.clone();
            let attempt = trace.start_attempt(&release_account_id);
            let response_result = create_response_with_account(
                AccountUpstreamContext {
                    codex: &self.codex,
                    account_identity: &self.account_identity,
                    cloudflare: &self.cloudflare,
                    request_id,
                    account: &account,
                },
                &attempt_request,
                started_at,
            )
            .await;
            acquired.complete().await;
            if let Err(error) = &response_result {
                self.cloudflare
                    .capture_set_cookie_headers(
                        &release_account_id,
                        upstream_error_set_cookie_headers(error),
                    )
                    .await;
            }

            match response_result {
                Ok(response) => {
                    self.cloudflare
                        .capture_set_cookie_headers(
                            &release_account_id,
                            &response.set_cookie_headers,
                        )
                        .await;
                    self.account_pool
                        .sync_passive_rate_limit_headers(&account, &response.rate_limit_headers)
                        .await;
                    let collected_response =
                        match response_from_codex_sse(&response.body, tuple_schema.as_ref()) {
                            Ok(collected_response) => collected_response,
                            Err(error) => {
                                let error = ResponseDispatchError::InvalidSse(error);
                                self.record_response_dispatch_error(
                                    request_id,
                                    route,
                                    requested_model,
                                    started_at,
                                    ResponseDispatchErrorDetails {
                                        client_api_key_id: request.client_api_key_id.as_deref(),
                                        account_id: Some(&release_account_id),
                                        stream: false,
                                        compact,
                                        transport: Some(backend_transport_name(response.transport)),
                                    },
                                    &error,
                                )
                                .await;
                                return Err(error);
                            }
                        };
                    if matches!(collected_response, CollectedResponse::Empty) {
                        self.account_pool
                            .record_empty_response_attempt(
                                &release_account_id,
                                image_generation_requested,
                            )
                            .await;
                        exhausted_accounts.record_upstream_unavailable(
                            Some(&release_account_id),
                            "upstream response did not include visible output",
                        );
                        if !history.can_failover() {
                            break (account, response, collected_response, attempt);
                        }
                        continue;
                    }
                    if let CollectedResponse::Failed(failure) = &collected_response {
                        if is_history_recovery_sse_failure(failure)
                            && history.recover_managed_history(&release_account_id)
                        {
                            next_required_account_id = Some(release_account_id);
                            continue;
                        }
                        if isolate_sse_account_failure(
                            self.account_pool.as_ref(),
                            &mut exhausted_accounts,
                            &release_account_id,
                            failure,
                        )
                        .await
                        {
                            if !history.can_failover() {
                                break (account, response, collected_response, attempt);
                            }
                            continue;
                        }
                    }
                    break (account, response, collected_response, attempt);
                }
                Err(error) => {
                    if isolate_rotatable_account_failure(
                        self.account_pool.as_ref(),
                        &self.cloudflare,
                        &mut exhausted_accounts,
                        &release_account_id,
                        &error,
                    )
                    .await
                    {
                        if !history.can_failover() {
                            return Err(ResponseDispatchError::Upstream(error));
                        }
                        continue;
                    }
                    if is_continuation_busy_error(&error) {
                        if history.recover_managed_history(&release_account_id) {
                            next_required_account_id = Some(release_account_id);
                            continue;
                        }
                        return Err(ResponseDispatchError::ContinuationBusy);
                    }
                    let history_unavailable = is_history_recovery_upstream_error(&error);
                    if history_unavailable && history.recover_managed_history(&release_account_id) {
                        next_required_account_id = Some(release_account_id);
                        continue;
                    }
                    record_response_upstream_error_event(ResponseUpstreamErrorEventRecord {
                        recorder: &self.recorder,
                        request_id,
                        account_id: &release_account_id,
                        account_email: account.email.as_deref(),
                        route,
                        model: requested_model,
                        started_at,
                        stream: false,
                        transport: backend_transport_for_response_request(&attempt_request),
                        request: &attempt_request,
                        error: &error,
                        trace: &trace,
                        attempt: Some(&attempt),
                    })
                    .await;
                    if is_retryable_account_transport_error(&error) {
                        if !history.can_failover() {
                            return Err(ResponseDispatchError::Upstream(error));
                        }
                        exhausted_accounts.record_upstream_unavailable(
                            Some(&release_account_id),
                            upstream_error_body(&error),
                        );
                        continue;
                    }
                    if history_unavailable {
                        if history.is_external_unknown() {
                            return Err(ResponseDispatchError::Upstream(error));
                        }
                        return Err(ResponseDispatchError::HistoryUnavailable {
                            upstream_error: upstream_error_body(&error),
                        });
                    }
                    return Err(ResponseDispatchError::Upstream(error));
                }
            }
        };

        let completed = matches!(&collected_response, CollectedResponse::Completed(_));
        match collected_response {
            CollectedResponse::Completed(body) | CollectedResponse::Incomplete(body) => {
                let response_id = body.get("id").and_then(Value::as_str);
                self.cloudflare.reset_account_recovery(&account.id).await;
                if let Some(usage) = response.usage {
                    self.account_pool
                        .record_response_usage(&account.id, usage, image_generation_requested)
                        .await;
                }
                if completed {
                    self.record_response_affinity(
                        &history,
                        &request,
                        &account.id,
                        &response.body,
                        response.turn_state.clone(),
                        response.usage,
                    )
                    .await;
                }
                let effective_model = response
                    .response_metadata
                    .effective_model
                    .as_deref()
                    .unwrap_or(&display_model);
                let mut metadata = json!({
                    "responseId": response_id,
                    "stream": false,
                    "completed": completed,
                    "incomplete": !completed,
                    "transport": backend_transport_name(response.transport),
                    "firstTokenMs": response.first_token_ms,
                    "usage": response.usage,
                    "effectiveModel": effective_model,
                    "modelsEtag": response.response_metadata.models_etag.as_deref(),
                    "reasoningIncluded": response.response_metadata.reasoning_included,
                });
                insert_response_status_metadata(
                    &mut metadata,
                    200,
                    200,
                    response.diagnostics.status_code.map(i64::from),
                );
                insert_response_upstream_diagnostics(&mut metadata, &response.diagnostics);
                insert_response_trace_metadata(&mut metadata, &trace, Some(&attempt));
                insert_websocket_pool_decision(&mut metadata, response.websocket_pool_decision);
                enrich_response_request_semantics(&mut metadata, &request);
                record_response_event(ResponseUsageRecord {
                    recorder: &self.recorder,
                    request_id,
                    client_api_key_id: request.client_api_key_id.as_deref(),
                    account_id: &account.id,
                    route,
                    model: effective_model,
                    requested_model: Some(requested_model),
                    client_ip: request.client_ip.as_deref(),
                    client_user_agent: request.client_user_agent.as_deref(),
                    reasoning_effort: reasoning_effort_from_request(&request),
                    service_tier: request.service_tier(),
                    started_at,
                    status_code: 200,
                    message: if completed {
                        "v1 responses completed"
                    } else {
                        "v1 responses incomplete"
                    },
                    metadata,
                    rate_limit_headers: &response.rate_limit_headers,
                })
                .await;
                self.models
                    .observe_models_etag(response.response_metadata.models_etag.as_deref());
                Ok(ResponseDispatchResponse {
                    body,
                    response_headers: response.response_metadata.client_headers,
                })
            }
            CollectedResponse::Failed(failure) => {
                let error = ResponseDispatchError::Failed(failure);
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: Some(&account.id),
                        stream: false,
                        compact,
                        transport: Some(backend_transport_name(response.transport)),
                    },
                    &error,
                )
                .await;
                Err(error)
            }
            CollectedResponse::MissingCompleted => {
                let error = ResponseDispatchError::MissingCompleted;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: Some(&account.id),
                        stream: false,
                        compact,
                        transport: Some(backend_transport_name(response.transport)),
                    },
                    &error,
                )
                .await;
                Err(error)
            }
            CollectedResponse::Empty => {
                let error = ResponseDispatchError::EmptyUpstreamResponse;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: Some(&account.id),
                        stream: false,
                        compact,
                        transport: Some(backend_transport_name(response.transport)),
                    },
                    &error,
                )
                .await;
                Err(error)
            }
        }
    }

    pub(super) async fn record_response_dispatch_error(
        &self,
        request_id: &str,
        route: &str,
        requested_model: &str,
        started_at: Instant,
        details: ResponseDispatchErrorDetails<'_>,
        error: &ResponseDispatchError,
    ) {
        record_response_dispatch_error_event(ResponseDispatchErrorEventRecord {
            recorder: &self.recorder,
            request_id,
            client_api_key_id: details.client_api_key_id,
            account_id: details.account_id,
            route,
            model: requested_model,
            started_at,
            stream: details.stream,
            compact: details.compact,
            transport: details.transport,
            error,
        })
        .await;
    }
}
