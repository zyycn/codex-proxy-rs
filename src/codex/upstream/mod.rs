pub mod dispatch;
pub mod fallback;
pub mod refresh;
pub mod stream;
pub mod usage;

use std::{sync::Arc, time::Instant};

use axum::{
    body::Body,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, Utc};
use futures::{stream as futures_stream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    codex::accounts::{
        model::{Account, AccountStatus},
        pool::{AccountAcquireRequest, AccountPool},
        repository::AccountRepository,
    },
    codex::cookies::repository::CookieRepository,
    codex::fingerprint::model::Fingerprint,
    codex::oauth::TokenRefresher,
    codex::protocol::codex_to_openai::openai_error,
    codex::transport::{
        client::{
            build_reqwest_client, CodexBackendClient, CodexBackendStream, CodexClientError,
            CodexRequestContext,
        },
        types::CodexResponsesRequest,
        usage::{extract_sse_usage, TokenUsage},
        websocket::{transport_for_request, CodexTransport},
    },
    config::AppConfig,
    logs::event::{EventLevel, EventLog},
    logs::repository::EventLogRepository,
};

use crate::http::v1::errors::codex_client_error_response;

pub(crate) use self::{
    dispatch::{no_available_accounts_response, normalize_service_tier_for_upstream},
    fallback::{
        classify_upstream_account_retry, websocket_history_retry_metadata, UpstreamAccountRetry,
    },
    stream::{completed_response_json, CollectedResponse},
};

use self::{
    dispatch::request_domain,
    refresh::refresh_account_after_unauthorized,
    stream::{ensure_stream_metadata, responses_sse_failure},
    usage::{record_request_attempt, record_usage_with_deps},
};

#[derive(Clone)]
struct CodexUpstreamDependencies {
    config: AppConfig,
    account_pool: Arc<Mutex<AccountPool>>,
    account_repository: Option<AccountRepository>,
    cookie_repository: Option<CookieRepository>,
    event_logs: Option<EventLogRepository>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
}

#[derive(Clone)]
pub(crate) struct CodexUpstreamService {
    deps: CodexUpstreamDependencies,
}

impl CodexUpstreamService {
    pub(crate) fn new(
        config: AppConfig,
        account_pool: Arc<Mutex<AccountPool>>,
        account_repository: Option<AccountRepository>,
        cookie_repository: Option<CookieRepository>,
        event_logs: Option<EventLogRepository>,
        token_refresher: Option<Arc<dyn TokenRefresher>>,
    ) -> Self {
        Self {
            deps: CodexUpstreamDependencies {
                config,
                account_pool,
                account_repository,
                cookie_repository,
                event_logs,
                token_refresher,
            },
        }
    }

    pub(crate) async fn acquire_account(&self, model: &str) -> Option<Account> {
        self.deps
            .account_pool
            .lock()
            .await
            .acquire_with(AccountAcquireRequest::new(model, Utc::now()))
            .map(|acquired| acquired.account)
    }

    pub(crate) async fn release_account(&self, account_id: &str) {
        self.deps.account_pool.lock().await.release(account_id);
    }

    pub(crate) async fn send_codex_request_with_refresh_retry(
        &self,
        request: &CodexResponsesRequest,
        account: &Account,
        request_id: &str,
    ) -> Result<crate::codex::transport::client::CodexBackendResponse, CodexClientError> {
        send_codex_request_with_refresh_retry_deps(&self.deps, request, account, request_id).await
    }

    pub(crate) async fn apply_retry_and_acquire_fallback(
        &self,
        account: &Account,
        retry: UpstreamAccountRetry,
        model: &str,
        excluded_account_ids: &mut Vec<String>,
    ) -> Option<Account> {
        apply_upstream_retry_and_acquire_fallback_with_deps(
            &self.deps,
            account,
            retry,
            model,
            excluded_account_ids,
        )
        .await
    }

    pub(crate) async fn apply_account_retry(&self, account: &Account, retry: UpstreamAccountRetry) {
        apply_upstream_account_retry_with_deps(&self.deps, account, retry).await;
    }

    pub(crate) async fn responses_stream(
        &self,
        request: CodexResponsesRequest,
        mut account: Account,
        mut log_context: CodexRequestLogContext,
    ) -> Response {
        let deps = self.deps.clone();
        if transport_for_request(&request) == CodexTransport::WebSocketRequired {
            return responses_websocket_stream(deps, request, account, log_context).await;
        }

        let mut excluded_account_ids = Vec::new();
        let stream_response = loop {
            let stream_response = send_codex_stream_request_with_refresh_retry(
                &deps,
                &request,
                &account,
                log_context.request_id.as_str(),
            )
            .await;
            match stream_response {
                Ok(response) => break response,
                Err(error) => {
                    deps.account_pool.lock().await.release(&account.id);
                    if let Some(retry) = classify_upstream_account_retry(&error) {
                        let fallback = apply_upstream_retry_and_acquire_fallback_with_deps(
                            &deps,
                            &account,
                            retry,
                            &request.model,
                            &mut excluded_account_ids,
                        )
                        .await;
                        log_codex_upstream_response_with_deps(
                            &deps,
                            &log_context,
                            retry.status(),
                            EventLevel::Warn,
                            "v1 responses stream upstream retrying with fallback account",
                            retry.metadata(true),
                        )
                        .await;
                        if let Some(fallback) = fallback {
                            account = fallback;
                            log_context = log_context.with_account(&account.id);
                            continue;
                        }
                    }
                    let error_response = codex_client_error_response(error);
                    log_codex_upstream_response_with_deps(
                        &deps,
                        &log_context,
                        error_response.0,
                        EventLevel::Error,
                        "v1 responses stream upstream request failed",
                        json!({"stream": true}),
                    )
                    .await;
                    return error_response.into_response();
                }
            }
        };

        if persist_upstream_cookies_with_deps(
            &deps,
            &account.id,
            &stream_response.set_cookie_headers,
        )
        .await
        .is_err()
        {
            deps.account_pool.lock().await.release(&account.id);
            log_codex_upstream_response_with_deps(
                &deps,
                &log_context,
                StatusCode::INTERNAL_SERVER_ERROR,
                EventLevel::Error,
                "v1 responses stream cookie persistence failed",
                json!({"stream": true, "cookieStoreError": true}),
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

        let upstream = Box::pin(stream_response.response.bytes_stream());
        let audit = StreamAudit::new(deps, log_context, account.id);
        let body_stream =
            futures_stream::unfold(Some((upstream, Vec::new(), audit)), |state| async move {
                let (mut upstream, mut collected, mut audit) = state?;
                match upstream.next().await {
                    Some(Ok(chunk)) => {
                        collected.extend_from_slice(&chunk);
                        Some((Ok(chunk), Some((upstream, collected, audit))))
                    }
                    Some(Err(error)) => {
                        audit.log_transport_error(&error).await;
                        Some((Err(error), None))
                    }
                    None => {
                        audit.complete(&collected).await;
                        None
                    }
                }
            });

        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/event-stream")
            .header(CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(body_stream))
            .unwrap_or_else(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(openai_error(
                        "Failed to build stream response",
                        "stream_response_error",
                    )),
                )
                    .into_response()
            })
    }

    pub(crate) async fn persist_cookies(
        &self,
        account_id: &str,
        set_cookie_headers: &[String],
    ) -> Result<(), ()> {
        persist_upstream_cookies_with_deps(&self.deps, account_id, set_cookie_headers).await
    }

    pub(crate) async fn record_usage(&self, account_id: &str, usage: TokenUsage) -> Result<(), ()> {
        record_usage_with_deps(&self.deps, account_id, usage).await
    }

    pub(crate) async fn log_response(
        &self,
        context: &CodexRequestLogContext,
        status: StatusCode,
        level: EventLevel,
        message: &str,
        metadata: Value,
    ) {
        log_codex_upstream_response_with_deps(
            &self.deps, context, status, level, message, metadata,
        )
        .await;
    }
}

async fn send_codex_request_with_refresh_retry_deps(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::transport::client::CodexBackendResponse, CodexClientError> {
    match send_codex_request(deps, request, account, request_id).await {
        Err(CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
        }) if status == StatusCode::UNAUTHORIZED => {
            let Some(refreshed) =
                refresh_account_after_unauthorized(deps, request, account, request_id).await
            else {
                return Err(CodexClientError::Upstream {
                    status,
                    body,
                    retry_after_seconds,
                });
            };
            send_codex_request(deps, request, &refreshed, request_id).await
        }
        result => result,
    }
}

async fn send_codex_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::transport::client::CodexBackendResponse, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };
    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        Fingerprint::default_codex_desktop(),
    );
    client
        .create_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: request.codex_window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
            },
        )
        .await
}

async fn send_codex_stream_request_with_refresh_retry(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendStream, CodexClientError> {
    match send_codex_stream_request(deps, request, account, request_id).await {
        Err(CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
        }) if status == StatusCode::UNAUTHORIZED => {
            let Some(refreshed) =
                refresh_account_after_unauthorized(deps, request, account, request_id).await
            else {
                return Err(CodexClientError::Upstream {
                    status,
                    body,
                    retry_after_seconds,
                });
            };
            send_codex_stream_request(deps, request, &refreshed, request_id).await
        }
        result => result,
    }
}

async fn send_codex_stream_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendStream, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };
    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        Fingerprint::default_codex_desktop(),
    );
    client
        .stream_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: request.codex_window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
            },
        )
        .await
}

async fn apply_upstream_retry_and_acquire_fallback_with_deps(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    retry: UpstreamAccountRetry,
    model: &str,
    excluded_account_ids: &mut Vec<String>,
) -> Option<Account> {
    apply_upstream_account_retry_with_deps(deps, account, retry).await;
    excluded_account_ids.push(account.id.clone());
    deps.account_pool
        .lock()
        .await
        .acquire_with(
            AccountAcquireRequest::new(model, Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned()),
        )
        .map(|fallback| fallback.account)
}

async fn apply_upstream_account_retry_with_deps(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    retry: UpstreamAccountRetry,
) {
    match retry {
        UpstreamAccountRetry::RateLimited {
            retry_after_seconds,
        } => {
            let cooldown_until = Utc::now() + Duration::seconds(retry_after_seconds as i64);
            if let Some(repo) = deps.account_repository.as_ref() {
                if repo
                    .set_quota_cooldown_until(&account.id, cooldown_until)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        account_id = %account.id,
                        "failed to persist quota cooldown"
                    );
                }
            }
            deps.account_pool
                .lock()
                .await
                .mark_quota_limited_until(&account.id, cooldown_until);
            if record_request_attempt(deps, &account.id).await.is_err() {
                tracing::warn!(
                    account_id = %account.id,
                    "failed to record rate-limited account attempt"
                );
            }
        }
        UpstreamAccountRetry::QuotaExhausted => {
            set_account_status(deps, account, AccountStatus::QuotaExhausted).await;
        }
        UpstreamAccountRetry::CloudflareChallenge { cooldown_seconds } => {
            let cooldown_until = Utc::now() + Duration::seconds(cooldown_seconds as i64);
            if let Some(cookie_repo) = deps.cookie_repository.as_ref() {
                if cookie_repo
                    .delete_account_cookies(&account.id)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        account_id = %account.id,
                        "failed to clear Cloudflare-blocked account cookies"
                    );
                }
            }
            if let Some(repo) = deps.account_repository.as_ref() {
                if repo
                    .set_cloudflare_cooldown_until(&account.id, cooldown_until)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        account_id = %account.id,
                        "failed to persist Cloudflare cooldown"
                    );
                }
            }
            deps.account_pool
                .lock()
                .await
                .set_cloudflare_cooldown_until(&account.id, cooldown_until);
        }
        UpstreamAccountRetry::Banned => {
            set_account_status(deps, account, AccountStatus::Banned).await;
        }
    }
}

async fn set_account_status(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    status: AccountStatus,
) {
    if let Some(repo) = deps.account_repository.as_ref() {
        if repo.set_status(&account.id, status).await.is_err() {
            tracing::warn!(
                account_id = %account.id,
                "failed to persist upstream account status"
            );
        }
    }
    deps.account_pool
        .lock()
        .await
        .set_status(&account.id, status);
}

async fn responses_websocket_stream(
    deps: CodexUpstreamDependencies,
    request: CodexResponsesRequest,
    mut account: Account,
    mut log_context: CodexRequestLogContext,
) -> Response {
    let mut excluded_account_ids = Vec::new();
    let response = loop {
        let response = send_codex_request_with_refresh_retry_deps(
            &deps,
            &request,
            &account,
            log_context.request_id.as_str(),
        )
        .await;
        deps.account_pool.lock().await.release(&account.id);

        match response {
            Ok(response) => break response,
            Err(error) => {
                if let Some(retry) = classify_upstream_account_retry(&error) {
                    if request.previous_response_id.is_some() {
                        // previous_response_id 的历史由上游账号持有，换账号会静默丢失会话上下文。
                        apply_upstream_account_retry_with_deps(&deps, &account, retry).await;
                        log_codex_upstream_response_with_deps(
                            &deps,
                            &log_context,
                            retry.status(),
                            EventLevel::Warn,
                            "v1 responses websocket history request kept on original account",
                            websocket_history_retry_metadata(retry, true),
                        )
                        .await;
                    } else {
                        let fallback = apply_upstream_retry_and_acquire_fallback_with_deps(
                            &deps,
                            &account,
                            retry,
                            &request.model,
                            &mut excluded_account_ids,
                        )
                        .await;
                        log_codex_upstream_response_with_deps(
                            &deps,
                            &log_context,
                            retry.status(),
                            EventLevel::Warn,
                            "v1 responses websocket upstream retrying with fallback account",
                            retry.metadata(true),
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
                log_codex_upstream_response_with_deps(
                    &deps,
                    &log_context,
                    error_response.0,
                    EventLevel::Error,
                    "v1 responses websocket stream upstream request failed",
                    json!({"stream": true, "transport": "websocket"}),
                )
                .await;
                return error_response.into_response();
            }
        }
    };

    if persist_upstream_cookies_with_deps(&deps, &account.id, &response.set_cookie_headers)
        .await
        .is_err()
    {
        log_codex_upstream_response_with_deps(
            &deps,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses websocket stream cookie persistence failed",
            json!({"stream": true, "transport": "websocket", "cookieStoreError": true}),
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

    let rate_limit_headers = response.rate_limit_headers.clone();
    let mut level = EventLevel::Info;
    let mut message = "v1 responses websocket stream completed";
    let mut metadata = json!({
        "stream": true,
        "transport": "websocket",
        "usage": response.usage,
        "rateLimitHeaders": rate_limit_headers.clone(),
    });
    if let Some(usage) = response.usage {
        if record_usage_with_deps(&deps, &account.id, usage)
            .await
            .is_err()
        {
            level = EventLevel::Warn;
            message = "v1 responses websocket stream completed with usage store error";
            metadata = json!({
                "stream": true,
                "transport": "websocket",
                "usage": usage,
                "rateLimitHeaders": rate_limit_headers.clone(),
                "usageStoreError": true,
            });
        }
    }
    log_codex_upstream_response_with_deps(
        &deps,
        &log_context,
        StatusCode::OK,
        level,
        message,
        metadata,
    )
    .await;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from(response.body))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to build stream response",
                    "stream_response_error",
                )),
            )
                .into_response()
        })
}

async fn persist_upstream_cookies_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
    set_cookie_headers: &[String],
) -> Result<(), ()> {
    let Some(cookie_repo) = deps.cookie_repository.as_ref() else {
        return Ok(());
    };
    for cookie in set_cookie_headers {
        cookie_repo
            .capture_set_cookie(account_id, cookie)
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct CodexRequestLogContext {
    request_id: String,
    account_id: String,
    model: String,
    stream: bool,
    started_at: Instant,
}

impl CodexRequestLogContext {
    pub(crate) fn new(
        request_id: &str,
        account_id: &str,
        model: &str,
        stream: bool,
        started_at: Instant,
    ) -> Self {
        Self {
            request_id: request_id.to_string(),
            account_id: account_id.to_string(),
            model: model.to_string(),
            stream,
            started_at,
        }
    }

    fn latency_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
    }

    pub(crate) fn with_account(&self, account_id: &str) -> Self {
        Self {
            request_id: self.request_id.clone(),
            account_id: account_id.to_string(),
            model: self.model.clone(),
            stream: self.stream,
            started_at: self.started_at,
        }
    }
}

struct StreamAudit {
    deps: CodexUpstreamDependencies,
    context: CodexRequestLogContext,
    account_slot: AccountSlotGuard,
}

impl StreamAudit {
    fn new(
        deps: CodexUpstreamDependencies,
        context: CodexRequestLogContext,
        account_id: String,
    ) -> Self {
        let account_slot = AccountSlotGuard::new(deps.account_pool.clone(), account_id);
        Self {
            deps,
            context,
            account_slot,
        }
    }

    async fn complete(&mut self, body: &[u8]) {
        let body = String::from_utf8_lossy(body);
        let mut status = StatusCode::OK;
        let mut level = EventLevel::Info;
        let mut message = "v1 responses stream completed";
        let mut metadata = match extract_sse_usage(&body) {
            Ok(usage) => {
                if let Some(usage) = usage {
                    if record_usage_with_deps(&self.deps, &self.context.account_id, usage)
                        .await
                        .is_err()
                    {
                        level = EventLevel::Warn;
                        message = "v1 responses stream completed with usage store error";
                        json!({"stream": true, "usage": usage, "usageStoreError": true})
                    } else {
                        json!({"stream": true, "usage": usage})
                    }
                } else {
                    json!({"stream": true, "usage": null})
                }
            }
            Err(error) => {
                level = EventLevel::Warn;
                message = "v1 responses stream completed with invalid SSE usage";
                json!({"stream": true, "sseParseError": error.to_string()})
            }
        };
        match responses_sse_failure(&body) {
            Ok(Some(failure)) => {
                // SSE 响应头已发出，HTTP 状态不能回滚；用终止事件透传给客户端，并在审计里标记上游失败。
                status = StatusCode::BAD_GATEWAY;
                level = EventLevel::Error;
                message = "v1 responses stream upstream SSE failed";
                failure.extend_metadata(&mut metadata);
            }
            Ok(None) => {}
            Err(error) => {
                level = EventLevel::Warn;
                message = "v1 responses stream completed with invalid SSE failure metadata";
                metadata = json!({"stream": true, "sseParseError": error.to_string()});
            }
        }
        ensure_stream_metadata(&mut metadata, true);
        log_codex_upstream_response_with_deps(
            &self.deps,
            &self.context,
            status,
            level,
            message,
            metadata,
        )
        .await;
        self.account_slot.release().await;
    }

    async fn log_transport_error(&mut self, error: &reqwest::Error) {
        log_codex_upstream_response_with_deps(
            &self.deps,
            &self.context,
            StatusCode::BAD_GATEWAY,
            EventLevel::Error,
            "v1 responses stream transport failed",
            json!({"stream": true, "transportError": error.to_string()}),
        )
        .await;
        self.account_slot.release().await;
    }
}

struct AccountSlotGuard {
    pool: Arc<Mutex<AccountPool>>,
    account_id: String,
    released: bool,
}

impl AccountSlotGuard {
    fn new(pool: Arc<Mutex<AccountPool>>, account_id: String) -> Self {
        Self {
            pool,
            account_id,
            released: false,
        }
    }

    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.pool.lock().await.release(&self.account_id);
        self.released = true;
    }
}

impl Drop for AccountSlotGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let pool = self.pool.clone();
        let account_id = self.account_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            pool.lock().await.release(&account_id);
        });
    }
}

async fn log_codex_upstream_response_with_deps(
    deps: &CodexUpstreamDependencies,
    context: &CodexRequestLogContext,
    status: StatusCode,
    level: EventLevel,
    message: &str,
    mut metadata: Value,
) {
    let Some(repo) = deps.event_logs.as_ref() else {
        return;
    };
    ensure_stream_metadata(&mut metadata, context.stream);
    let mut event = EventLog::new("v1.response", level, message);
    event.request_id = Some(context.request_id.clone());
    event.account_id = Some(context.account_id.clone());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(context.model.clone());
    event.status_code = Some(i64::from(status.as_u16()));
    event.latency_ms = Some(context.latency_ms());
    event.metadata = metadata;
    if let Err(error) = repo.insert(event).await {
        tracing::warn!(?error, "failed to insert v1 response event log");
    }
}
