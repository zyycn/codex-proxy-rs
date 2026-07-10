use super::*;

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
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let account_affinity = self.account_affinity_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(request.model(), now);
        if let Some(preferred_account_id) = account_affinity.preferred_account_id() {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut next_required_account_id: Option<String> = None;
        let mut quota_verify_attempts = 0usize;
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
            let mut attempt_acquire_request = acquire_request
                .clone()
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            if let Some(account_id) = next_required_account_id.take() {
                attempt_acquire_request =
                    attempt_acquire_request.with_required_account_id(account_id);
            }
            let allow_quota_verify_account_retry =
                attempt_acquire_request.required_account_id.is_none();
            attempt_acquire_request.now = Utc::now();
            let Some(acquired) = self
                .account_pool
                .acquire_with(&attempt_acquire_request)
                .await
            else {
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
                    installation_id: self.installation_id.as_deref(),
                    request_id,
                    excluded_account_ids: &mut excluded_account_ids,
                    verify_attempts: &mut quota_verify_attempts,
                    allow_retry_with_another_account: allow_quota_verify_account_retry,
                },
                acquired,
            )
            .await
            {
                QuotaVerificationDecision::Ready(acquired) => *acquired,
                QuotaVerificationDecision::RetryWithAnotherAccount => {
                    exhausted_accounts
                        .record_rate_limited(None, QUOTA_VERIFY_LIMIT_REACHED_MESSAGE);
                    continue;
                }
                QuotaVerificationDecision::MaxAttemptsReached
                | QuotaVerificationDecision::RequiredAccountUnavailable => {
                    exhausted_accounts.record_rate_limited(
                        Some(&acquired_account_id),
                        QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
                    );
                    let error = exhausted_accounts
                        .last_exhausted()
                        .map(ResponseDispatchError::from_exhausted_account)
                        .unwrap_or(ResponseDispatchError::NoActiveAccount);
                    return_stream_dispatch_error!(
                        error,
                        account_id: Some(&acquired_account_id),
                        transport: Some(backend_transport_name(backend_transport_for_response_request(
                            &request
                        )))
                    );
                }
            };

            self.apply_cascading_ban_defense(
                &mut request,
                &mut implicit_resume,
                account_affinity.preferred_account_id(),
                &acquired.account.id,
            )
            .await;
            Self::strip_history_if_account_changed(
                &mut request,
                &mut implicit_resume,
                &account_affinity,
                &acquired.account.id,
            );

            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let attempt = trace.start_attempt(&release_account_id);
            let response_result = create_response_stream_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
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
                    self.cloudflare
                        .capture_set_cookie_headers(&release_account_id, &set_cookie_headers)
                        .await;
                    self.account_pool
                        .sync_passive_rate_limit_headers(&account, &rate_limit_headers)
                        .await;
                    let (prefetched, body) = match prefetch_first_sse_chunk(response.body).await {
                        Ok(prefetched) => prefetched,
                        Err(ResponseDispatchError::Upstream(error))
                            if is_rate_limit_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_rate_limited(
                                Some(&release_account_id),
                                upstream_error_body(&error),
                            );
                            let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                            self.account_pool
                                .mark_quota_limited_until(&release_account_id, cooldown_until)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_quota_exhausted_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_quota_exhausted(
                                Some(&release_account_id),
                                upstream_error_body(&error),
                            );
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_auth_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            let upstream_error = upstream_error_body(&error);
                            let account_status = auth_failure_account_status(&error);
                            exhausted_accounts.record_auth_failure(
                                Some(&release_account_id),
                                account_status,
                                upstream_error,
                                Some(upstream_error_http_status(&error)),
                            );
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_cloudflare_challenge_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_cloudflare_challenge(
                                Some(&release_account_id),
                                cloudflare_challenge_error_message(),
                            );
                            self.cloudflare
                                .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_cloudflare_path_block_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_cloudflare_path_blocked(
                                Some(&release_account_id),
                                cloudflare_path_block_error_message(),
                            );
                            self.cloudflare
                                .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_model_unsupported_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            let upstream_error = upstream_error_body(&error);
                            if let Some(exhausted) = exhausted_accounts
                                .model_unsupported_retry_exhausted(upstream_error.clone())
                            {
                                return_stream_dispatch_error!(
                                    ResponseDispatchError::from_exhausted_account(exhausted),
                                    account_id: Some(&release_account_id),
                                    transport: Some(backend_transport_name(transport))
                                );
                            }
                            exhausted_accounts.record_model_unsupported(
                                Some(&release_account_id),
                                upstream_error,
                            );
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_banned_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_auth_failure(
                                Some(&release_account_id),
                                AccountStatus::Banned,
                                upstream_error_body(&error),
                                Some(upstream_error_http_status(&error)),
                            );
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::Banned)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(error) => {
                            self.account_pool.release(&release_account_id).await;
                            if let ResponseDispatchError::Upstream(upstream_error) = &error {
                                let history_unavailable =
                                    is_history_recovery_upstream_error(upstream_error);
                                if history_unavailable
                                    && self
                                        .try_recover_implicit_resume(
                                            &mut request,
                                            &mut implicit_resume,
                                            &release_account_id,
                                            client_error_invalid_reasoning_replay(upstream_error),
                                        )
                                        .await
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
                                        request: &request,
                                        error: upstream_error,
                                        trace: &trace,
                                        attempt: Some(&attempt),
                                    },
                                )
                                .await;
                                if history_unavailable {
                                    return Err(ResponseDispatchError::HistoryUnavailable {
                                        upstream_error: upstream_error_body(upstream_error),
                                    });
                                }
                                return Err(error);
                            }
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
                            self.account_pool.release(&release_account_id).await;
                            return_stream_dispatch_error!(
                                ResponseDispatchError::InvalidSse(error),
                                account_id: Some(&release_account_id),
                                transport: Some(backend_transport_name(transport))
                            );
                        }
                    };
                    if let Some(failure) = first_failure {
                        if is_history_recovery_sse_failure(&failure)
                            && self
                                .try_recover_implicit_resume(
                                    &mut request,
                                    &mut implicit_resume,
                                    &release_account_id,
                                    sse_failure_invalid_reasoning_replay(&failure),
                                )
                                .await
                        {
                            self.account_pool.release(&release_account_id).await;
                            next_required_account_id = Some(release_account_id);
                            continue;
                        }
                        if is_model_unsupported_sse_failure(&failure) {
                            let upstream_error = sse_failure_error_body(&failure);
                            if let Some(exhausted) = exhausted_accounts
                                .model_unsupported_retry_exhausted(upstream_error.clone())
                            {
                                self.account_pool.release(&release_account_id).await;
                                return_stream_dispatch_error!(
                                    ResponseDispatchError::from_exhausted_account(exhausted),
                                    account_id: Some(&release_account_id),
                                    transport: Some(backend_transport_name(transport))
                                );
                            }
                            exhausted_accounts.record_model_unsupported(
                                Some(&release_account_id),
                                upstream_error,
                            );
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(&failure) {
                            exhausted_accounts.record_quota_exhausted(
                                Some(&release_account_id),
                                failure.message.clone(),
                            );
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        if is_auth_sse_failure(&failure) {
                            let upstream_error = sse_failure_error_body(&failure);
                            let account_status = auth_sse_failure_account_status(&failure);
                            exhausted_accounts.record_auth_failure(
                                Some(&release_account_id),
                                account_status,
                                upstream_error,
                                Some(stream_failure_http_status(&failure)),
                            );
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        self.account_pool.release(&release_account_id).await;
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
                                request: &request,
                                failure: &failure,
                                diagnostics: &diagnostics,
                                rate_limit_headers: &rate_limit_headers,
                                prefetched: &prefetched,
                                trace: &trace,
                                attempt: &attempt,
                            },
                        )
                        .await;
                        let error = if is_history_recovery_sse_failure(&failure) {
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
                        session_affinity: Arc::clone(&self.session_affinity),
                        reasoning_replay: Arc::clone(&self.reasoning_replay),
                        recorder: Arc::clone(&self.recorder),
                        cloudflare: self.cloudflare.clone(),
                        account_id: account.id,
                        account_plan_type: account.plan_type,
                        request_id: request_id.to_string(),
                        route: route.to_string(),
                        model: request.model().to_string(),
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
                        attempt: attempt.clone(),
                        attempts: trace.attempts().to_vec(),
                        started_at,
                    };
                    return Ok(spawn_live_response_stream(context, prefetched, body));
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    exhausted_accounts.record_rate_limited(
                        Some(&release_account_id),
                        upstream_error_body(&error),
                    );
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    exhausted_accounts.record_quota_exhausted(
                        Some(&release_account_id),
                        upstream_error_body(&error),
                    );
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    let upstream_error = upstream_error_body(&error);
                    let account_status = auth_failure_account_status(&error);
                    exhausted_accounts.record_auth_failure(
                        Some(&release_account_id),
                        account_status,
                        upstream_error,
                        Some(upstream_error_http_status(&error)),
                    );
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    exhausted_accounts.record_cloudflare_challenge(
                        Some(&release_account_id),
                        cloudflare_challenge_error_message(),
                    );
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    exhausted_accounts.record_cloudflare_path_blocked(
                        Some(&release_account_id),
                        cloudflare_path_block_error_message(),
                    );
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    let upstream_error = upstream_error_body(&error);
                    if let Some(exhausted) =
                        exhausted_accounts.model_unsupported_retry_exhausted(upstream_error.clone())
                    {
                        return_stream_dispatch_error!(
                            ResponseDispatchError::from_exhausted_account(exhausted),
                            account_id: Some(&release_account_id),
                            transport: Some(backend_transport_name(backend_transport_for_response_request(
                                &request
                            )))
                        );
                    }
                    exhausted_accounts
                        .record_model_unsupported(Some(&release_account_id), upstream_error);
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    exhausted_accounts.record_auth_failure(
                        Some(&release_account_id),
                        AccountStatus::Banned,
                        upstream_error_body(&error),
                        Some(upstream_error_http_status(&error)),
                    );
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::Banned)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) => {
                    self.account_pool.release(&release_account_id).await;
                    let history_unavailable = is_history_recovery_upstream_error(&error);
                    if history_unavailable
                        && self
                            .try_recover_implicit_resume(
                                &mut request,
                                &mut implicit_resume,
                                &release_account_id,
                                client_error_invalid_reasoning_replay(&error),
                            )
                            .await
                    {
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
                        transport: backend_transport_for_response_request(&request),
                        request: &request,
                        error: &error,
                        trace: &trace,
                        attempt: Some(&attempt),
                    })
                    .await;
                    if history_unavailable {
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
