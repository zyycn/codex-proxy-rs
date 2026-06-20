use super::dispatch_responses::*;
use super::*;

/// OpenAI Chat Completions 调度服务。
#[derive(Clone)]
pub struct ChatDispatchService {
    account_pool: Arc<RuntimeAccountPoolService>,
    models: Arc<ModelService>,
    codex: Arc<CodexBackendClient>,
    logs: Arc<AdminLogService>,
    installation_id: Option<String>,
    cloudflare: CloudflareRecovery,
}

impl ChatDispatchService {
    /// 构造 Chat Completions 调度服务。
    pub(crate) fn new(
        account_pool: Arc<RuntimeAccountPoolService>,
        models: Arc<ModelService>,
        codex: Arc<CodexBackendClient>,
        logs: Arc<AdminLogService>,
        installation_id: Option<String>,
        cloudflare: CloudflareRecovery,
    ) -> Self {
        Self {
            account_pool,
            models,
            codex,
            logs,
            installation_id,
            cloudflare,
        }
    }

    /// 调度非流式 Chat Completions 请求到 Codex Responses 上游。
    pub async fn complete(
        &self,
        request_id: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<Value, ChatDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let parsed_model = catalog.parse_model_name(requested_model);
        let display_model =
            codex_proxy_core::models::catalog::ModelCatalog::build_display_model_name(
                &parsed_model,
            );
        apply_response_model_options(&mut request, &parsed_model, self.models.config());
        let include_reasoning = request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.get("effort"))
            .and_then(Value::as_str)
            .is_some_and(|effort| !effort.trim().is_empty());
        let tuple_schema = request.tuple_schema.clone();
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut disabled_count = 0usize;
        let mut last_disabled_auth_error = None;
        let mut banned_count = 0usize;
        let mut last_banned_auth_error = None;
        let mut last_banned_status_code: Option<u16> = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;
        let mut quota_verify_attempts = 0usize;
        let mut last_failed_account_id = None;
        macro_rules! return_dispatch_error {
            ($error:expr) => {{
                let error = $error;
                self.record_chat_dispatch_error(
                    request_id,
                    requested_model,
                    started_at,
                    last_failed_account_id.as_deref(),
                    Some(backend_transport_name(requested_response_transport(
                        &request,
                    ))),
                    &error,
                )
                .await;
                return Err(error);
            }};
            ($error:expr, account_id: $account_id:expr, transport: $transport:expr) => {{
                let error = $error;
                self.record_chat_dispatch_error(
                    request_id,
                    requested_model,
                    started_at,
                    $account_id,
                    $transport,
                    &error,
                )
                .await;
                return Err(error);
            }};
        }
        let (account_id, response) = loop {
            let acquire_request = AccountAcquireRequest::new(&request.model, Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            let acquired = match self.account_pool.acquire_with(acquire_request).await {
                Some(acquired) => acquired,
                None if quota_exhausted_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::QuotaExhausted {
                        count: quota_exhausted_count,
                        upstream_error: last_quota_error.unwrap_or_default(),
                    });
                }
                None if rate_limited_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::RateLimited {
                        count: rate_limited_count,
                        upstream_error: last_rate_limit_error.unwrap_or_default(),
                    });
                }
                None if expired_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::Expired {
                        count: expired_count,
                        upstream_error: last_auth_error.unwrap_or_default(),
                    });
                }
                None if disabled_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::Disabled {
                        count: disabled_count,
                        upstream_error: last_disabled_auth_error.unwrap_or_default(),
                    });
                }
                None if banned_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::Banned {
                        count: banned_count,
                        upstream_error: last_banned_auth_error.unwrap_or_default(),
                        status_code: last_banned_status_code.unwrap_or(403),
                    });
                }
                None if cloudflare_challenge_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::CloudflareChallenge {
                        count: cloudflare_challenge_count,
                        upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                    });
                }
                None if cloudflare_path_block_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::CloudflarePathBlocked {
                        count: cloudflare_path_block_count,
                        upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                    });
                }
                None if model_unsupported_count > 0 => {
                    return_dispatch_error!(ChatDispatchError::ModelUnsupported {
                        count: model_unsupported_count,
                        upstream_error: last_model_unsupported_error.unwrap_or_default(),
                    });
                }
                None => return_dispatch_error!(ChatDispatchError::NoActiveAccount),
            };
            let acquired = match verify_acquired_quota_if_required(
                self.account_pool.as_ref(),
                self.codex.as_ref(),
                &self.cloudflare,
                self.installation_id.as_deref(),
                request_id,
                acquired,
                &mut excluded_account_ids,
                &mut quota_verify_attempts,
            )
            .await
            {
                QuotaVerificationDecision::Ready(acquired) => *acquired,
                QuotaVerificationDecision::RetryWithAnotherAccount => {
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string());
                    continue;
                }
                QuotaVerificationDecision::MaxAttemptsReached => {
                    return_dispatch_error!(ChatDispatchError::RateLimited {
                        count: rate_limited_count + 1,
                        upstream_error: QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string(),
                    });
                }
            };
            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let response_result = create_response_with_account(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
            )
            .await;
            self.account_pool.release(&release_account_id).await;

            match response_result {
                Ok(response) => break (release_account_id, response),
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    last_failed_account_id = Some(release_account_id.clone());
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    last_failed_account_id = Some(release_account_id.clone());
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    let account_status = auth_failure_account_status(&error);
                    last_failed_account_id = Some(release_account_id.clone());
                    match account_status {
                        AccountStatus::Disabled => {
                            disabled_count += 1;
                            last_disabled_auth_error = Some(upstream_error);
                        }
                        AccountStatus::Banned => {
                            banned_count += 1;
                            last_banned_status_code = Some(upstream_error_http_status(&error));
                            last_banned_auth_error = Some(upstream_error);
                        }
                        _ => {
                            expired_count += 1;
                            last_auth_error = Some(upstream_error);
                        }
                    }
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    last_failed_account_id = Some(release_account_id.clone());
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    last_failed_account_id = Some(release_account_id.clone());
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        return_dispatch_error!(
                            ChatDispatchError::ModelUnsupported {
                                count: model_unsupported_count + 1,
                                upstream_error,
                            },
                            account_id: Some(&release_account_id),
                            transport: Some(backend_transport_name(requested_response_transport(
                                &request
                            )))
                        );
                    }
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    last_failed_account_id = Some(release_account_id.clone());
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    banned_count += 1;
                    last_banned_status_code = Some(upstream_error_http_status(&error));
                    last_banned_auth_error = Some(upstream_error_body(&error));
                    last_failed_account_id = Some(release_account_id.clone());
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::Banned)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) => {
                    return_dispatch_error!(
                        ChatDispatchError::Upstream(error),
                        account_id: Some(&release_account_id),
                        transport: Some(backend_transport_name(requested_response_transport(
                            &request
                        )))
                    );
                }
            }
        };
        let body = match chat_completion_from_codex_sse(
            &response.body,
            &display_model,
            include_reasoning,
            tuple_schema.as_ref(),
        ) {
            Ok(Some(body)) => body,
            Ok(None) => {
                return_dispatch_error!(
                    ChatDispatchError::EmptyUpstreamResponse,
                    account_id: Some(&account_id),
                    transport: Some(backend_transport_name(response.transport))
                );
            }
            Err(error) => {
                return_dispatch_error!(
                    ChatDispatchError::InvalidSse(error),
                    account_id: Some(&account_id),
                    transport: Some(backend_transport_name(response.transport))
                );
            }
        };
        let response_id = body.get("id").and_then(Value::as_str);
        self.cloudflare.reset_account_recovery(&account_id).await;
        if let Some(usage) = response.usage {
            self.account_pool
                .record_token_usage(&account_id, usage)
                .await;
        }
        record_response_event(ResponseEventRecord {
            logs: &self.logs,
            request_id,
            account_id: &account_id,
            route: "/v1/chat/completions",
            model: requested_model,
            started_at,
            status_code: 200,
            level: EventLevel::Info,
            message: "v1 chat completions completed",
            metadata: serde_json::json!({
                "responseId": response_id,
                "stream": false,
                "transport": backend_transport_name(response.transport),
                "usage": response.usage,
            }),
            rate_limit_headers: &response.rate_limit_headers,
        })
        .await;
        Ok(body)
    }

    async fn record_chat_dispatch_error(
        &self,
        request_id: &str,
        requested_model: &str,
        started_at: Instant,
        account_id: Option<&str>,
        transport: Option<&str>,
        error: &ChatDispatchError,
    ) {
        record_chat_dispatch_error_event(ChatDispatchErrorEventRecord {
            logs: &self.logs,
            request_id,
            account_id,
            route: "/v1/chat/completions",
            model: requested_model,
            started_at,
            transport,
            error,
        })
        .await;
    }
}

/// Chat Completions 调度错误。
#[derive(Debug, Error)]
pub enum ChatDispatchError {
    /// 账号存储失败。
    #[error("failed to list runtime accounts")]
    AccountStore,
    /// 没有活跃账号。
    #[error("no active account is available")]
    NoActiveAccount,
    /// 所有账号都因配额耗尽不可用。
    #[error("all accounts exhausted by quota")]
    QuotaExhausted {
        /// 配额耗尽账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因限流不可用。
    #[error("all accounts exhausted by rate limit")]
    RateLimited {
        /// 限流账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因认证失效不可用。
    #[error("all accounts exhausted by expired auth")]
    Expired {
        /// 认证失效账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因 token 已确认不可用而被禁用。
    #[error("all accounts exhausted by disabled auth")]
    Disabled {
        /// 禁用账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都被上游封禁。
    #[error("all accounts exhausted by banned auth")]
    Banned {
        /// 封禁账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
        /// 应返回给客户端的上游 HTTP 状态。
        status_code: u16,
    },
    /// 所有账号都因 Cloudflare challenge 暂不可用。
    #[error("all accounts exhausted by Cloudflare challenge")]
    CloudflareChallenge {
        /// Cloudflare challenge 账号数量。
        count: usize,
        /// 最后一个上游错误说明。
        upstream_error: String,
    },
    /// 所有账号都因 Cloudflare path-block 暂不可用。
    #[error("all accounts exhausted by Cloudflare path-block")]
    CloudflarePathBlocked {
        /// Cloudflare path-block 账号数量。
        count: usize,
        /// 最后一个上游错误说明。
        upstream_error: String,
    },
    /// 所有账号都不支持请求模型。
    #[error("all accounts do not support the requested model")]
    ModelUnsupported {
        /// 不支持模型的账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 上游请求失败。
    #[error("upstream request failed: {0}")]
    Upstream(#[from] CodexClientError),
    /// 上游 SSE 无法解析。
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 上游响应没有完成事件。
    #[error("upstream response did not include response.completed")]
    EmptyUpstreamResponse,
}

impl ChatDispatchError {
    /// HTTP status code that best represents this dispatch failure to API clients.
    #[must_use]
    pub fn http_status_code(&self) -> u16 {
        match self {
            Self::NoActiveAccount | Self::AccountStore => 503,
            Self::QuotaExhausted { .. } => 402,
            Self::RateLimited { .. } => 429,
            Self::Expired { .. } | Self::Disabled { .. } => 401,
            Self::Banned { status_code, .. } => *status_code,
            Self::CloudflareChallenge { .. }
            | Self::CloudflarePathBlocked { .. }
            | Self::InvalidSse(_)
            | Self::EmptyUpstreamResponse => 502,
            Self::ModelUnsupported { .. } => 400,
            Self::Upstream(error) => upstream_error_http_status(error),
        }
    }
}
