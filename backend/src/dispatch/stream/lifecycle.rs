use std::{sync::Arc, time::Instant};

use chrono::Utc;

use crate::{
    dispatch::{
        affinity::prepare_variant_identity,
        attempts::AccountAttemptLedger,
        errors::{
            backend_transport_name, is_continuation_busy_error, is_history_recovery_upstream_error,
            is_retryable_account_transport_error, upstream_error_body,
            upstream_error_set_cookie_headers, ResponseDispatchError,
        },
        recording::{
            record_prefetched_response_stream_failure_event, record_response_upstream_error_event,
            ResponseDispatchErrorDetails, ResponseStreamFailureEventRecord,
            ResponseUpstreamErrorEventRecord,
        },
        recovery::{
            account_failure::{isolate_rotatable_account_failure, isolate_sse_account_failure},
            exhaustion::AccountExhaustionTracker,
            history::HistoryRecoveryPlan,
        },
        service::{ResponseDispatchService, ResponseDispatchStream},
        stream::{
            live::{spawn_live_response_stream, LiveResponseStreamContext},
            prefetch::prefetch_first_sse_chunk,
            sse_failure::{
                first_sse_failure, is_history_recovery_sse_failure, sse_failure_error_body,
            },
            trace::ResponseDispatchTrace,
        },
        upstream_call::{
            create_response_stream_with_account_retrying_5xx, verify_acquired_quota_if_required,
            AccountUpstreamContext, QuotaVerificationContext, QuotaVerificationDecision,
            QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
        },
    },
    fleet::pool::AccountAcquireRequest,
    upstream::openai::{
        protocol::responses::CodexResponsesRequest,
        transport::backend_transport_for_response_request,
    },
};

impl ResponseDispatchService {
    /// 调度流式 Responses 请求到 Codex Responses 上游。
    pub async fn stream(
        &self,
        request_id: &str,
        route: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<ResponseDispatchStream, ResponseDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        request.set_stream(true);
        let tuple_schema = request.tuple_schema.clone();
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
        macro_rules! return_stream_dispatch_error {
            ($error:expr) => {{
                let error = $error;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: exhausted_accounts.last_account_id(),
                        stream: true,
                        compact: false,
                        transport: Some(backend_transport_name(
                            backend_transport_for_response_request(&request),
                        )),
                    },
                    &error,
                )
                .await;
                return Err(error);
            }};
            ($error:expr, account_id: $account_id:expr, transport: $transport:expr) => {{
                let error = $error;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: $account_id,
                        stream: true,
                        compact: false,
                        transport: $transport,
                    },
                    &error,
                )
                .await;
                return Err(error);
            }};
        }
        loop {
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
                    "Responses stream account candidate ledger exhausted"
                );
                let error = exhausted_accounts
                    .last_exhausted()
                    .map(ResponseDispatchError::from_exhausted_account)
                    .unwrap_or(ResponseDispatchError::NoActiveAccount);
                return_stream_dispatch_error!(error);
            };
            let acquired_account_id = acquired.account.id.clone();
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
                return_stream_dispatch_error!(ResponseDispatchError::HistoryUnavailable {
                    upstream_error: "previous response history cannot be sent to another account"
                        .to_string(),
                });
            };

            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account.clone();
            let release_account_id = account.id.clone();
            let attempt = trace.start_attempt(&release_account_id);
            let response_result = create_response_stream_with_account_retrying_5xx(
                AccountUpstreamContext {
                    codex: &self.codex,
                    account_identity: &self.account_identity,
                    cloudflare: &self.cloudflare,
                    request_id,
                    account: &account,
                },
                &attempt_request,
            )
            .await;
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
                    let transport = response.transport;
                    let set_cookie_headers = response.set_cookie_headers;
                    let rate_limit_headers = response.rate_limit_headers;
                    let rate_limit_header_updates = response.rate_limit_header_updates;
                    let turn_state_update = response.turn_state_update;
                    let websocket_pool_decision = response.websocket_pool_decision;
                    let turn_state = response.turn_state;
                    let diagnostics = response.diagnostics;
                    let response_metadata = response.response_metadata;
                    self.models
                        .observe_models_etag(response_metadata.models_etag.as_deref());
                    self.cloudflare
                        .capture_set_cookie_headers(&release_account_id, &set_cookie_headers)
                        .await;
                    self.account_pool
                        .sync_passive_rate_limit_headers(&account, &rate_limit_headers)
                        .await;
                    let (prefetched, body) = match prefetch_first_sse_chunk(response.body).await {
                        Ok(prefetched) => prefetched,
                        Err(ResponseDispatchError::Upstream(error)) => {
                            acquired.complete().await;
                            if isolate_rotatable_account_failure(
                                self.account_pool.as_ref(),
                                &self.cloudflare,
                                &mut exhausted_accounts,
                                &release_account_id,
                                &error,
                            )
                            .await
                            {
                                if history.is_external_unknown() {
                                    return_stream_dispatch_error!(
                                        ResponseDispatchError::Upstream(error),
                                        account_id: Some(&release_account_id),
                                        transport: Some(backend_transport_name(transport))
                                    );
                                }
                                continue;
                            }
                            if is_continuation_busy_error(&error) {
                                if history.recover_managed_history(&release_account_id) {
                                    next_required_account_id = Some(release_account_id);
                                    continue;
                                }
                                return_stream_dispatch_error!(
                                    ResponseDispatchError::ContinuationBusy,
                                    account_id: Some(&release_account_id),
                                    transport: Some(backend_transport_name(transport))
                                );
                            }
                            let history_unavailable = is_history_recovery_upstream_error(&error);
                            if history_unavailable
                                && history.recover_managed_history(&release_account_id)
                            {
                                next_required_account_id = Some(release_account_id);
                                continue;
                            }
                            record_response_upstream_error_event(
                                ResponseUpstreamErrorEventRecord {
                                    recorder: &self.recorder,
                                    request_id,
                                    account_id: &release_account_id,
                                    account_email: account.email.as_deref(),
                                    route,
                                    model: requested_model,
                                    started_at,
                                    stream: true,
                                    transport,
                                    request: &attempt_request,
                                    error: &error,
                                    trace: &trace,
                                    attempt: Some(&attempt),
                                },
                            )
                            .await;
                            if is_retryable_account_transport_error(&error) {
                                exhausted_accounts.record_upstream_unavailable(
                                    Some(&release_account_id),
                                    upstream_error_body(&error),
                                );
                                if history.is_external_unknown() {
                                    return Err(ResponseDispatchError::Upstream(error));
                                }
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
                        Err(error) => {
                            acquired.complete().await;
                            return_stream_dispatch_error!(
                                error,
                                account_id: Some(&release_account_id),
                                transport: Some(backend_transport_name(transport))
                            );
                        }
                    };
                    let first_failure = match first_sse_failure(&prefetched) {
                        Ok(failure) => failure,
                        Err(error) => {
                            acquired.complete().await;
                            return_stream_dispatch_error!(
                                ResponseDispatchError::InvalidSse(error),
                                account_id: Some(&release_account_id),
                                transport: Some(backend_transport_name(transport))
                            );
                        }
                    };
                    if let Some(failure) = first_failure {
                        if is_history_recovery_sse_failure(&failure)
                            && history.recover_managed_history(&release_account_id)
                        {
                            acquired.complete().await;
                            next_required_account_id = Some(release_account_id);
                            continue;
                        }
                        if isolate_sse_account_failure(
                            self.account_pool.as_ref(),
                            &mut exhausted_accounts,
                            &release_account_id,
                            &failure,
                        )
                        .await
                        {
                            acquired.complete().await;
                            if history.is_external_unknown() {
                                return_stream_dispatch_error!(
                                    ResponseDispatchError::Failed(failure),
                                    account_id: Some(&release_account_id),
                                    transport: Some(backend_transport_name(transport))
                                );
                            }
                            continue;
                        }
                        acquired.complete().await;
                        record_prefetched_response_stream_failure_event(
                            ResponseStreamFailureEventRecord {
                                recorder: &self.recorder,
                                request_id,
                                account_id: &release_account_id,
                                route,
                                model: &display_model,
                                requested_model,
                                started_at,
                                transport,
                                request: &attempt_request,
                                failure: &failure,
                                diagnostics: &diagnostics,
                                rate_limit_headers: &rate_limit_headers,
                                prefetched: &prefetched,
                                trace: &trace,
                                attempt: &attempt,
                            },
                        )
                        .await;
                        let error = if is_history_recovery_sse_failure(&failure)
                            && !history.is_external_unknown()
                        {
                            ResponseDispatchError::HistoryUnavailable {
                                upstream_error: sse_failure_error_body(&failure),
                            }
                        } else {
                            ResponseDispatchError::Failed(failure.clone())
                        };
                        return Err(error);
                    }

                    let context = LiveResponseStreamContext {
                        account_pool: Arc::clone(&self.account_pool),
                        account_lease: acquired,
                        session_affinity: Arc::clone(&self.session_affinity),
                        history,
                        recorder: Arc::clone(&self.recorder),
                        cloudflare: self.cloudflare.clone(),
                        account_id: account.id,
                        account_plan_type: account.plan_type,
                        request_id: request_id.to_string(),
                        route: route.to_string(),
                        display_model: display_model.clone(),
                        requested_model: requested_model.to_string(),
                        client_ip: request.client_ip.clone(),
                        request,
                        tuple_schema,
                        transport,
                        rate_limit_headers,
                        rate_limit_header_updates,
                        turn_state_update,
                        websocket_pool_decision,
                        turn_state,
                        diagnostics,
                        response_metadata,
                        attempt: attempt.clone(),
                        attempts: trace.attempts().to_vec(),
                        started_at,
                    };
                    return Ok(spawn_live_response_stream(context, prefetched, body));
                }
                Err(error) => {
                    acquired.complete().await;
                    if isolate_rotatable_account_failure(
                        self.account_pool.as_ref(),
                        &self.cloudflare,
                        &mut exhausted_accounts,
                        &release_account_id,
                        &error,
                    )
                    .await
                    {
                        if history.is_external_unknown() {
                            return_stream_dispatch_error!(
                                ResponseDispatchError::Upstream(error),
                                account_id: Some(&release_account_id),
                                transport: Some(backend_transport_name(
                                    backend_transport_for_response_request(&attempt_request)
                                ))
                            );
                        }
                        continue;
                    }
                    if is_continuation_busy_error(&error) {
                        if history.recover_managed_history(&release_account_id) {
                            next_required_account_id = Some(release_account_id);
                            continue;
                        }
                        return_stream_dispatch_error!(
                            ResponseDispatchError::ContinuationBusy,
                            account_id: Some(&release_account_id),
                            transport: Some(backend_transport_name(
                                backend_transport_for_response_request(&attempt_request)
                            ))
                        );
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
                        stream: true,
                        transport: backend_transport_for_response_request(&attempt_request),
                        request: &attempt_request,
                        error: &error,
                        trace: &trace,
                        attempt: Some(&attempt),
                    })
                    .await;
                    if is_retryable_account_transport_error(&error) {
                        exhausted_accounts.record_upstream_unavailable(
                            Some(&release_account_id),
                            upstream_error_body(&error),
                        );
                        if history.is_external_unknown() {
                            return Err(ResponseDispatchError::Upstream(error));
                        }
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
        }
    }
}
