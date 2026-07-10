use super::*;

impl ResponseDispatchService {
    /// 调度 Responses compact 请求到 Codex compact 上游。
    pub async fn compact(
        &self,
        request_id: &str,
        mut request: CodexCompactRequest,
        requested_model: &str,
    ) -> Result<Value, ResponseDispatchError> {
        let started_at = Instant::now();
        let client_api_key_id = request.client_api_key_id.clone();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        let mut excluded_account_ids = Vec::new();
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut quota_verify_attempts = 0usize;
        let mut trace = ResponseDispatchTrace::default();

        loop {
            let acquire_request = AccountAcquireRequest::new(request.model(), Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            let acquired = match self.account_pool.acquire_with(&acquire_request).await {
                Some(acquired) => acquired,
                None => {
                    let error = exhausted_accounts
                        .last_exhausted()
                        .map(ResponseDispatchError::from_exhausted_account)
                        .unwrap_or(ResponseDispatchError::NoActiveAccount);
                    self.record_compact_dispatch_error(
                        request_id,
                        client_api_key_id.as_deref(),
                        requested_model,
                        started_at,
                        exhausted_accounts.last_account_id(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
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
                    allow_retry_with_another_account: true,
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
                    self.record_compact_dispatch_error(
                        request_id,
                        client_api_key_id.as_deref(),
                        requested_model,
                        started_at,
                        Some(&acquired_account_id),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            };
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let attempt = trace.start_attempt(&release_account_id);
            let response_result = create_compact_response_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
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
                    self.cloudflare.reset_account_recovery(&account.id).await;
                    self.account_pool
                        .sync_passive_rate_limit_headers(&account, &response.rate_limit_headers)
                        .await;
                    let usage = extract_usage(&response.body);
                    if let Some(usage) = usage {
                        self.account_pool
                            .record_token_usage(&account.id, request.model(), &usage)
                            .await;
                    }
                    let mut metadata = json!({
                        "stream": false,
                        "compact": true,
                        "usage": usage,
                    });
                    insert_response_status_metadata(
                        &mut metadata,
                        200,
                        200,
                        response.diagnostics.status_code.map(i64::from),
                    );
                    insert_response_upstream_diagnostics(&mut metadata, &response.diagnostics);
                    insert_response_trace_metadata(&mut metadata, &trace, Some(&attempt));
                    record_response_event(ResponseUsageRecord {
                        usage_records: &self.usage_records,
                        request_id,
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: &account.id,
                        route: "/v1/responses/compact",
                        model: &display_model,
                        requested_model: Some(requested_model),
                        client_ip: request.client_ip.as_deref(),
                        client_user_agent: request.client_user_agent.as_deref(),
                        reasoning_effort: reasoning_effort_from_compact_request(&request),
                        service_tier: None,
                        started_at,
                        status_code: 200,
                        message: "v1 responses compact completed",
                        metadata,
                        rate_limit_headers: &response.rate_limit_headers,
                    })
                    .await;
                    return Ok(response.body);
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
                        self.record_compact_dispatch_error(
                            request_id,
                            client_api_key_id.as_deref(),
                            requested_model,
                            started_at,
                            Some(&release_account_id),
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
                    let error = ResponseDispatchError::Upstream(error);
                    self.record_compact_dispatch_error(
                        request_id,
                        client_api_key_id.as_deref(),
                        requested_model,
                        started_at,
                        Some(&release_account_id),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            }
        }
    }

    async fn record_compact_dispatch_error(
        &self,
        request_id: &str,
        client_api_key_id: Option<&str>,
        requested_model: &str,
        started_at: Instant,
        account_id: Option<&str>,
        error: &ResponseDispatchError,
    ) {
        record_response_dispatch_error_event(ResponseDispatchErrorEventRecord {
            ops_errors: &self.ops_errors,
            request_id,
            client_api_key_id,
            account_id,
            route: "/v1/responses/compact",
            model: requested_model,
            started_at,
            stream: false,
            compact: true,
            transport: Some("http"),
            error,
        })
        .await;
    }
}
