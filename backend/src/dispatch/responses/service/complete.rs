use super::*;

impl ResponseDispatchService {
    /// 调度非流式 Responses 请求到 Codex Responses 上游。
    pub async fn complete(
        &self,
        request_id: &str,
        route: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<Value, ResponseDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        let tuple_schema = request.tuple_schema.clone();
        let image_generation_requested = request.expects_image_generation();
        let now = Utc::now();
        let explicit_previous_response_id = request.previous_response_id().map(ToString::to_string);
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let account_affinity = self.account_affinity_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(request.model(), now);
        if let Some(preferred_account_id) = account_affinity.preferred_account_id() {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut history_recovery_used = false;
        let mut next_required_account_id: Option<String> = None;
        let mut empty_response_retries = 0u8;
        let mut quota_verify_attempts = 0usize;
        let mut trace = ResponseDispatchTrace::default();
        const MAX_EMPTY_RESPONSE_RETRIES: u8 = 2;
        let (account, response, collected_response, attempt): (
            Account,
            CodexBackendResponse,
            CollectedResponse,
            ResponseDispatchAttempt,
        ) = loop {
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
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: exhausted_accounts.last_account_id(),
                        stream: false,
                        compact: false,
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
                    self.record_response_dispatch_error(
                        request_id,
                        route,
                        requested_model,
                        started_at,
                        ResponseDispatchErrorDetails {
                            client_api_key_id: request.client_api_key_id.as_deref(),
                            account_id: Some(&acquired_account_id),
                            stream: false,
                            compact: false,
                            transport: Some(backend_transport_name(
                                backend_transport_for_response_request(&request),
                            )),
                        },
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            };

            self.apply_cascading_ban_defense(
                &mut request,
                &mut implicit_resume,
                account_affinity.preferred_account_id(),
                &acquired.account.id,
                explicit_previous_response_id.as_deref(),
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
            let response_result = create_response_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
                started_at,
            )
            .await;
            self.account_pool.release(&release_account_id).await;
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
                                        compact: false,
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
                                request.model(),
                                image_generation_requested,
                            )
                            .await;
                        empty_response_retries += 1;
                        if empty_response_retries <= MAX_EMPTY_RESPONSE_RETRIES {
                            continue;
                        }
                    }
                    if let CollectedResponse::Failed(failure) = &collected_response {
                        if is_history_recovery_sse_failure(failure) && !history_recovery_used {
                            if sse_failure_invalid_reasoning_replay(failure) {
                                self.evict_reasoning_replay(&request, &release_account_id)
                                    .await;
                            }
                            self.recover_request_history(&mut request, &mut implicit_resume)
                                .await;
                            history_recovery_used = true;
                            next_required_account_id = Some(release_account_id);
                            continue;
                        }
                        if is_model_unsupported_sse_failure(failure) {
                            let upstream_error = sse_failure_error_body(failure);
                            if let Some(exhausted) = exhausted_accounts
                                .model_unsupported_retry_exhausted(upstream_error.clone())
                            {
                                let error =
                                    ResponseDispatchError::from_exhausted_account(exhausted);
                                self.record_response_dispatch_error(
                                    request_id,
                                    route,
                                    requested_model,
                                    started_at,
                                    ResponseDispatchErrorDetails {
                                        client_api_key_id: request.client_api_key_id.as_deref(),
                                        account_id: Some(&release_account_id),
                                        stream: false,
                                        compact: false,
                                        transport: Some(backend_transport_name(response.transport)),
                                    },
                                    &error,
                                )
                                .await;
                                return Err(error);
                            }
                            exhausted_accounts.record_model_unsupported(
                                Some(&release_account_id),
                                upstream_error,
                            );
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(failure) {
                            exhausted_accounts.record_quota_exhausted(
                                Some(&release_account_id),
                                failure.message.clone(),
                            );
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_auth_sse_failure(failure) {
                            let upstream_error = sse_failure_error_body(failure);
                            let account_status = auth_sse_failure_account_status(failure);
                            exhausted_accounts.record_auth_failure(
                                Some(&release_account_id),
                                account_status,
                                upstream_error,
                                Some(stream_failure_http_status(failure)),
                            );
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                    }
                    break (account, response, collected_response, attempt);
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
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
                    exhausted_accounts.record_quota_exhausted(
                        Some(&release_account_id),
                        upstream_error_body(&error),
                    );
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error)
                    if is_history_recovery_upstream_error(&error) && !history_recovery_used =>
                {
                    if client_error_invalid_reasoning_replay(&error) {
                        self.evict_reasoning_replay(&request, &release_account_id)
                            .await;
                    }
                    self.recover_request_history(&mut request, &mut implicit_resume)
                        .await;
                    history_recovery_used = true;
                    next_required_account_id = Some(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
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
                    let upstream_error = upstream_error_body(&error);
                    if let Some(exhausted) =
                        exhausted_accounts.model_unsupported_retry_exhausted(upstream_error.clone())
                    {
                        let error = ResponseDispatchError::from_exhausted_account(exhausted);
                        self.record_response_dispatch_error(
                            request_id,
                            route,
                            requested_model,
                            started_at,
                            ResponseDispatchErrorDetails {
                                client_api_key_id: request.client_api_key_id.as_deref(),
                                account_id: Some(&release_account_id),
                                stream: false,
                                compact: false,
                                transport: Some(backend_transport_name(
                                    backend_transport_for_response_request(&request),
                                )),
                            },
                            &error,
                        )
                        .await;
                        return Err(error);
                    }
                    exhausted_accounts
                        .record_model_unsupported(Some(&release_account_id), upstream_error);
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
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
                    record_response_upstream_error_event(ResponseUpstreamErrorEventRecord {
                        recorder: &self.recorder,
                        request_id,
                        account_id: &release_account_id,
                        account_email: account.email.as_deref(),
                        route,
                        model: requested_model,
                        started_at,
                        stream: false,
                        transport: backend_transport_for_response_request(&request),
                        request: &request,
                        error: &error,
                        trace: &trace,
                        attempt: Some(&attempt),
                    })
                    .await;
                    return Err(ResponseDispatchError::Upstream(error));
                }
            }
        };

        match collected_response {
            CollectedResponse::Completed(body) => {
                let response_id = body.get("id").and_then(Value::as_str);
                self.cloudflare.reset_account_recovery(&account.id).await;
                if let Some(usage) = response.usage {
                    self.account_pool
                        .record_response_usage(
                            &account.id,
                            request.model(),
                            usage,
                            image_generation_requested,
                        )
                        .await;
                }
                self.record_response_affinity(
                    &request,
                    &account.id,
                    &response.body,
                    response.turn_state.clone(),
                    response.usage,
                )
                .await;
                let mut metadata = json!({
                    "responseId": response_id,
                    "stream": false,
                    "transport": backend_transport_name(response.transport),
                    "firstTokenMs": response.first_token_ms,
                    "usage": response.usage,
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
                record_response_event(ResponseUsageRecord {
                    recorder: &self.recorder,
                    request_id,
                    client_api_key_id: request.client_api_key_id.as_deref(),
                    account_id: &account.id,
                    route,
                    model: &display_model,
                    requested_model: Some(requested_model),
                    client_ip: request.client_ip.as_deref(),
                    client_user_agent: request.client_user_agent.as_deref(),
                    reasoning_effort: reasoning_effort_from_request(&request),
                    service_tier: request.service_tier(),
                    started_at,
                    status_code: 200,
                    message: "v1 responses completed",
                    metadata,
                    rate_limit_headers: &response.rate_limit_headers,
                })
                .await;
                Ok(body)
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
                        compact: false,
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
                        compact: false,
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
                        compact: false,
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
