//! OpenAI Chat Completions 调度服务。

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use serde_json::Value;
use thiserror::Error;

use crate::{
    admin::monitoring::{
        usage_record_model::{ResponseUsageRecord, UsageRecordLevel},
        usage_record_service::AdminUsageRecordService,
    },
    proxy::dispatch::{
        auth_recovery::trigger_refresh_after_auth_failure,
        cloudflare::{
            cloudflare_challenge_error_message, cloudflare_path_block_error_message,
            is_cloudflare_challenge_upstream_error, is_cloudflare_path_block_upstream_error,
            CloudflareRecovery,
        },
        errors::{
            auth_failure_account_status, backend_transport_name, is_auth_upstream_error,
            is_model_unsupported_upstream_error, is_quota_exhausted_upstream_error,
            is_rate_limit_upstream_error, rate_limit_cooldown_until, upstream_error_body,
            upstream_error_http_status,
        },
        upstream::{
            create_response_with_account, verify_acquired_quota_if_required,
            QuotaVerificationContext, QuotaVerificationDecision,
            QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
        },
        usage_events::{
            reasoning_effort_from_request, record_dispatch_error_event, record_response_event,
            DispatchErrorUsageRecord,
        },
    },
    proxy::openai::chat::ChatStreamTranslationError,
    upstream::accounts::{
        model::AccountStatus,
        pool::{AccountAcquireRequest, RuntimeAccountPoolService},
        token_refresh::RuntimeTokenRefreshService,
    },
    upstream::{
        models::ModelCatalog,
        protocol::responses::{apply_response_model_options, CodexResponsesRequest},
        token_client::OpenAiTokenClient,
        transport::{
            backend_transport_for_response_request, is_banned_upstream_error, CodexBackendClient,
            CodexClientError,
        },
    },
};

/// OpenAI Chat Completions 调度服务。
#[derive(Clone)]
pub struct ChatDispatchService {
    account_pool: Arc<RuntimeAccountPoolService>,
    models: Arc<crate::upstream::models::ModelService>,
    codex: Arc<CodexBackendClient>,
    usage_records: Arc<AdminUsageRecordService>,
    token_refresh: Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
    installation_id: Option<String>,
    cloudflare: CloudflareRecovery,
}

impl ChatDispatchService {
    pub(crate) fn new(
        account_pool: Arc<RuntimeAccountPoolService>,
        models: Arc<crate::upstream::models::ModelService>,
        codex: Arc<CodexBackendClient>,
        usage_records: Arc<AdminUsageRecordService>,
        token_refresh: Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
        installation_id: Option<String>,
        cloudflare: CloudflareRecovery,
    ) -> Self {
        Self {
            account_pool,
            models,
            codex,
            usage_records,
            token_refresh,
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
        let display_model = ModelCatalog::build_display_model_name(&parsed_model);
        apply_response_model_options(&mut request, &parsed_model);
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
                    Some(backend_transport_name(
                        backend_transport_for_response_request(&request),
                    )),
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

        let (account, response) = loop {
            let acquire_request = AccountAcquireRequest::new(&request.model, Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            let acquired = match self.account_pool.acquire_with(&acquire_request).await {
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
                QuotaVerificationContext {
                    account_pool: self.account_pool.as_ref(),
                    codex: self.codex.as_ref(),
                    cloudflare: &self.cloudflare,
                    installation_id: self.installation_id.as_deref(),
                    request_id,
                    excluded_account_ids: &mut excluded_account_ids,
                    verify_attempts: &mut quota_verify_attempts,
                },
                acquired,
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
                started_at,
            )
            .await;
            self.account_pool.release(&release_account_id).await;

            match response_result {
                Ok(response) => break (account, response),
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
                    trigger_refresh_after_auth_failure(
                        &self.token_refresh,
                        &release_account_id,
                        account_status,
                    );
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
                            transport: Some(backend_transport_name(backend_transport_for_response_request(
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
                        transport: Some(backend_transport_name(backend_transport_for_response_request(
                            &request
                        )))
                    );
                }
            }
        };
        let account_id = account.id.clone();
        let body = match crate::proxy::openai::chat::chat_completion_from_codex_sse(
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
        self.account_pool
            .sync_passive_rate_limit_headers(&account, &response.rate_limit_headers)
            .await;
        if let Some(ref usage) = response.usage {
            self.account_pool
                .record_token_usage(&account_id, &request.model, usage)
                .await;
        }
        let mut metadata = serde_json::json!({
            "responseId": response_id,
            "stream": false,
            "transport": backend_transport_name(response.transport),
            "firstTokenMs": response.first_token_ms,
            "usage": response.usage,
        });
        if let (Some(object), Some(decision)) =
            (metadata.as_object_mut(), response.websocket_pool_decision)
        {
            object.insert("websocketPool".to_string(), decision.metadata_value());
        }
        record_response_event(ResponseUsageRecord {
            usage_records: &self.usage_records,
            request_id,
            account_id: &account_id,
            route: "/v1/chat/completions",
            model: &display_model,
            requested_model: Some(requested_model),
            client_ip: request.client_ip.as_deref(),
            client_user_agent: request.client_user_agent.as_deref(),
            reasoning_effort: reasoning_effort_from_request(&request),
            service_tier: request.service_tier.as_deref(),
            started_at,
            status_code: 200,
            level: UsageRecordLevel::Info,
            message: "v1 chat completions completed",
            metadata,
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
            usage_records: &self.usage_records,
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
    #[error("failed to list runtime accounts")]
    AccountStore,
    #[error("no active account is available")]
    NoActiveAccount,
    #[error("all accounts exhausted by quota")]
    QuotaExhausted {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by rate limit")]
    RateLimited {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by expired auth")]
    Expired {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by disabled auth")]
    Disabled {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by banned auth")]
    Banned {
        count: usize,
        upstream_error: String,
        status_code: u16,
    },
    #[error("all accounts exhausted by Cloudflare challenge")]
    CloudflareChallenge {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by Cloudflare path-block")]
    CloudflarePathBlocked {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts do not support the requested model")]
    ModelUnsupported {
        count: usize,
        upstream_error: String,
    },
    #[error("upstream request failed: {0}")]
    Upstream(#[from] CodexClientError),
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(ChatStreamTranslationError),
    #[error("upstream response did not include response.completed")]
    EmptyUpstreamResponse,
}

impl ChatDispatchError {
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

// ====================================================================
// Event recording helpers
// ====================================================================

struct ChatDispatchErrorEventRecord<'a> {
    usage_records: &'a AdminUsageRecordService,
    request_id: &'a str,
    account_id: Option<&'a str>,
    route: &'a str,
    model: &'a str,
    started_at: Instant,
    transport: Option<&'a str>,
    error: &'a ChatDispatchError,
}

async fn record_chat_dispatch_error_event(record: ChatDispatchErrorEventRecord<'_>) {
    let mut metadata = serde_json::json!({
        "route": record.route,
        "apiKind": "chat",
        "stream": false,
        "failed": true,
        "errorKind": "dispatch",
        "error": record.error.to_string(),
    });
    if let Some(object) = metadata.as_object_mut() {
        enrich_chat_error_metadata(object, record.error);
        if let Some(transport) = record.transport {
            object.insert("transport".to_string(), serde_json::json!(transport));
        }
    }
    record_dispatch_error_event(DispatchErrorUsageRecord {
        usage_records: record.usage_records,
        request_id: record.request_id,
        account_id: record.account_id,
        route: record.route,
        model: record.model,
        started_at: record.started_at,
        status_code: i64::from(record.error.http_status_code()),
        message: "chat dispatch failed",
        metadata,
    })
    .await;
}

fn enrich_chat_error_metadata(
    object: &mut serde_json::Map<String, Value>,
    error: &ChatDispatchError,
) {
    let (failure_class, exhausted_count, upstream_error, upstream_status) = match error {
        ChatDispatchError::AccountStore => ("account_store", None, None, None),
        ChatDispatchError::NoActiveAccount => ("no_available_accounts", None, None, None),
        ChatDispatchError::QuotaExhausted {
            count,
            upstream_error,
        } => (
            "quota_exhausted",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ChatDispatchError::RateLimited {
            count,
            upstream_error,
        } => (
            "rate_limited",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ChatDispatchError::Expired {
            count,
            upstream_error,
        } => ("expired", Some(*count), Some(upstream_error.clone()), None),
        ChatDispatchError::Disabled {
            count,
            upstream_error,
        } => ("disabled", Some(*count), Some(upstream_error.clone()), None),
        ChatDispatchError::Banned {
            count,
            upstream_error,
            ..
        } => ("banned", Some(*count), Some(upstream_error.clone()), None),
        ChatDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        } => (
            "cloudflare_challenge",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ChatDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        } => (
            "cloudflare_path_blocked",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ChatDispatchError::ModelUnsupported {
            count,
            upstream_error,
        } => (
            "model_unsupported",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ChatDispatchError::Upstream(error) => {
            let upstream_status = match error {
                CodexClientError::Upstream { status, .. } => Some(status.as_u16()),
                _ => None,
            };
            (
                "upstream",
                None,
                Some(upstream_error_body(error)),
                upstream_status,
            )
        }
        ChatDispatchError::InvalidSse(_) => ("invalid_sse", None, None, None),
        ChatDispatchError::EmptyUpstreamResponse => ("empty_upstream_response", None, None, None),
    };

    object.insert(
        "failureClass".to_string(),
        Value::String(failure_class.to_string()),
    );
    if let Some(count) = exhausted_count {
        object.insert("exhaustedCount".to_string(), serde_json::json!(count));
    }
    if let Some(error) = upstream_error {
        object.insert("upstreamError".to_string(), Value::String(error));
    }
    if let Some(status) = upstream_status {
        object.insert("upstreamStatus".to_string(), serde_json::json!(status));
    }
}
