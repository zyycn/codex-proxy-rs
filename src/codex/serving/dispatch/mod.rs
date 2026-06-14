mod account_refresh;
pub mod affinity;
pub mod fallback;
mod limits;
// 上游调度辅助命名为 routing，避免出现 dispatch::dispatch 的模块套娃。
pub mod routing;
pub mod stream;
mod stream_audit;
pub mod usage;

use std::{
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use axum::{
    body::Body,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use futures::{stream as futures_stream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    codex::accounts::cookies::repository::CookieRepository,
    codex::accounts::{
        model::Account,
        pool::{AccountAcquireRequest, AccountPool, AcquiredAccount},
        repository::AccountRepository,
    },
    codex::events::{
        event::{EventLevel, EventLog},
        service::LogService,
    },
    codex::gateway::conversation_identity::{build_conversation_identity, ensure_prompt_cache_key},
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::installation_id::get_installation_id,
    codex::gateway::oauth::TokenRefresher,
    codex::gateway::protocol::codex_to_openai::openai_error,
    codex::gateway::transport::{
        http_client::{
            build_reqwest_client, CodexBackendClient, CodexBackendStream,
            CodexBackendWebSocketStream, CodexClientError, CodexRequestContext,
        },
        types::CodexResponsesRequest,
        usage_events::TokenUsage,
        websocket::{
            transport_for_request, CodexTransport, CodexWebSocketError, CodexWebSocketPool,
        },
    },
    config::AppConfig,
};

use crate::codex::serving::http::errors::codex_client_error_response;

pub(crate) use self::{
    fallback::{
        classify_upstream_account_retry, websocket_history_retry_metadata, UpstreamAccountRetry,
    },
    routing::{no_available_accounts_response, normalize_service_tier_for_upstream},
    stream::{completed_response_json, CollectedResponse},
};

use self::affinity::{compute_variant_hash, SessionAffinityMap, SessionAffinityRepository};
use self::fallback::{
    apply_upstream_account_retry_with_deps, apply_upstream_retry_and_acquire_fallback_with_deps,
};
use self::stream_audit::{StreamAudit, WebSocketStreamAudit};
use self::{
    account_refresh::refresh_account_after_unauthorized,
    limits::apply_rate_limit_headers_with_deps,
    routing::request_domain,
    stream::{completed_response_metadata, ensure_stream_metadata},
    usage::{record_empty_response_with_deps, record_usage_with_deps},
};

#[derive(Clone)]
struct CodexUpstreamDependencies {
    config: Arc<AppConfig>,
    account_pool: Arc<Mutex<AccountPool>>,
    account_repository: Option<AccountRepository>,
    cookie_repository: Option<CookieRepository>,
    logs: LogService,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    fingerprint: Fingerprint, // 用于实际请求的指纹
    session_affinity: Arc<SessionAffinityMap>,
    session_affinity_repository: Option<SessionAffinityRepository>,
    websocket_pool: Arc<CodexWebSocketPool>,
}

#[derive(Clone)]
pub(crate) struct CodexUpstreamService {
    deps: CodexUpstreamDependencies,
}

pub(crate) struct CodexUpstreamRepositories {
    pub(crate) account: Option<AccountRepository>,
    pub(crate) cookie: Option<CookieRepository>,
    pub(crate) session_affinity: Option<SessionAffinityRepository>,
}

impl CodexUpstreamService {
    pub(crate) fn new(
        config: Arc<AppConfig>,
        account_pool: Arc<Mutex<AccountPool>>,
        repositories: CodexUpstreamRepositories,
        logs: LogService,
        token_refresher: Option<Arc<dyn TokenRefresher>>,
        fingerprint: Fingerprint,
        websocket_pool: Arc<CodexWebSocketPool>,
    ) -> Self {
        Self {
            deps: CodexUpstreamDependencies {
                config,
                account_pool,
                account_repository: repositories.account,
                cookie_repository: repositories.cookie,
                logs,
                token_refresher,
                fingerprint,
                session_affinity: Arc::new(SessionAffinityMap::with_default_ttl()),
                session_affinity_repository: repositories.session_affinity,
                websocket_pool,
            },
        }
    }

    pub(crate) async fn acquire_account(&self, model: &str) -> Option<AcquiredAccount> {
        self.deps
            .account_pool
            .lock()
            .await
            .acquire_with(AccountAcquireRequest::new(model, Utc::now()))
    }

    pub(crate) async fn prepare_response_session(&self, request: &mut CodexResponsesRequest) {
        if let Some(previous_response_id) = request.previous_response_id.as_deref() {
            if let Some(conversation_id) = self
                .deps
                .session_affinity
                .lookup_conversation_id(previous_response_id)
                .await
            {
                request.prompt_cache_key = Some(conversation_id);
            }
            if request
                .turn_state
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                request.turn_state = self
                    .deps
                    .session_affinity
                    .lookup_turn_state(previous_response_id)
                    .await;
            }
        }
        ensure_prompt_cache_key(request);
    }

    pub(crate) async fn acquire_account_for_request(
        &self,
        request: &CodexResponsesRequest,
    ) -> Option<AcquiredAccount> {
        let preferred_account_id = match request.previous_response_id.as_deref() {
            Some(previous_response_id) => {
                self.deps
                    .session_affinity
                    .lookup_account(previous_response_id)
                    .await
            }
            None => None,
        };
        let mut acquire_request = AccountAcquireRequest::new(&request.model, Utc::now());
        if let Some(preferred_account_id) = preferred_account_id {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        self.deps
            .account_pool
            .lock()
            .await
            .acquire_with(acquire_request)
    }

    pub(crate) async fn release_account(&self, account_id: &str) {
        self.deps.account_pool.lock().await.release(account_id);
    }

    pub(crate) async fn stagger_request(&self, previous_slot_at: Option<DateTime<Utc>>) {
        stagger_request_with_deps(&self.deps, previous_slot_at).await;
    }

    /// 获取当前使用的指纹（用于诊断）
    pub(crate) fn fingerprint(&self) -> &Fingerprint {
        &self.deps.fingerprint
    }

    pub(crate) async fn send_codex_request_with_refresh_retry(
        &self,
        request: &CodexResponsesRequest,
        account: &Account,
        request_id: &str,
    ) -> Result<crate::codex::gateway::transport::http_client::CodexBackendResponse, CodexClientError>
    {
        send_codex_request_with_refresh_retry_deps(&self.deps, request, account, request_id).await
    }

    pub(crate) async fn apply_retry_and_acquire_fallback(
        &self,
        account: &Account,
        retry: UpstreamAccountRetry,
        model: &str,
        excluded_account_ids: &mut Vec<String>,
    ) -> Option<AcquiredAccount> {
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
        acquired: AcquiredAccount,
        log_context: CodexRequestLogContext,
    ) -> Response {
        let deps = self.deps.clone();
        if matches!(
            transport_for_request(&request),
            CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired
        ) {
            return responses_websocket_stream(deps, request, acquired, log_context).await;
        }

        responses_http_sse_stream(deps, request, acquired, log_context).await
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

    pub(crate) async fn record_empty_response(&self, account_id: &str) -> Result<(), ()> {
        record_empty_response_with_deps(&self.deps, account_id).await
    }

    pub(crate) async fn record_response_affinity(
        &self,
        request: &CodexResponsesRequest,
        account_id: &str,
        body: &str,
        turn_state: Option<&str>,
        usage: Option<TokenUsage>,
    ) {
        record_response_affinity_with_deps(
            &self.deps, request, account_id, body, turn_state, usage,
        )
        .await;
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

    pub(crate) async fn reload_session_affinity_from_repository(
        &self,
    ) -> self::affinity::SessionAffinityRepositoryResult<usize> {
        let Some(repository) = self.deps.session_affinity_repository.as_ref() else {
            return Ok(0);
        };
        let now = Utc::now();
        repository.delete_expired(now).await?;
        let records = repository.list_active(now).await?;
        Ok(self.deps.session_affinity.restore(records).await)
    }
}

async fn stagger_request_with_deps(
    deps: &CodexUpstreamDependencies,
    previous_slot_at: Option<DateTime<Utc>>,
) {
    let Some(previous_slot_at) = previous_slot_at else {
        return;
    };
    let interval_ms = deps.config.auth.request_interval_ms;
    if interval_ms == 0 {
        return;
    }
    let elapsed_ms = Utc::now()
        .signed_duration_since(previous_slot_at)
        .num_milliseconds()
        .max(0) as u64;
    let Some(wait_ms) = interval_ms.checked_sub(elapsed_ms) else {
        return;
    };
    if wait_ms == 0 {
        return;
    }
    tracing::debug!(
        wait_ms,
        request_interval_ms = interval_ms,
        "按账户请求间隔等待后发送上游请求"
    );
    tokio::time::sleep(StdDuration::from_millis(wait_ms)).await;
}

async fn responses_http_sse_stream(
    deps: CodexUpstreamDependencies,
    request: CodexResponsesRequest,
    mut acquired: AcquiredAccount,
    mut log_context: CodexRequestLogContext,
) -> Response {
    let mut excluded_account_ids = Vec::new();
    let stream_response = loop {
        stagger_request_with_deps(&deps, acquired.previous_slot_at).await;
        let stream_response = send_codex_stream_request_with_refresh_retry(
            &deps,
            &request,
            &acquired.account,
            log_context.request_id.as_str(),
        )
        .await;
        match stream_response {
            Ok(response) => break response,
            Err(error) => {
                deps.account_pool.lock().await.release(&acquired.account.id);
                if let Some(retry) = classify_upstream_account_retry(&error) {
                    let fallback = apply_upstream_retry_and_acquire_fallback_with_deps(
                        &deps,
                        &acquired.account,
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
                        "v1 responses stream 上游请求将使用备用账户重试",
                        retry.metadata(true),
                    )
                    .await;
                    if let Some(fallback) = fallback {
                        log_context = log_context.with_account(&fallback.account.id);
                        acquired = fallback;
                        continue;
                    }
                }
                let error_response = codex_client_error_response(error);
                log_codex_upstream_response_with_deps(
                    &deps,
                    &log_context,
                    error_response.0,
                    EventLevel::Error,
                    "v1 responses stream 上游请求失败",
                    json!({"stream": true}),
                )
                .await;
                return error_response.into_response();
            }
        }
    };

    if persist_upstream_cookies_with_deps(
        &deps,
        &acquired.account.id,
        &stream_response.set_cookie_headers,
    )
    .await
    .is_err()
    {
        deps.account_pool.lock().await.release(&acquired.account.id);
        log_codex_upstream_response_with_deps(
            &deps,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses stream 持久化 cookie 失败",
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

    let rate_limit_headers = stream_response.rate_limit_headers.clone();
    let upstream = Box::pin(stream_response.response.bytes_stream());
    let audit = StreamAudit::new(
        deps,
        log_context,
        acquired.account.id,
        acquired.account.plan_type,
        rate_limit_headers,
    );

    use tokio::time::{interval, Duration};
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
    const HEARTBEAT_CHUNK: &[u8] = b": ping\n\n";

    let body_stream = futures_stream::unfold(
        Some((upstream, Vec::new(), audit, interval(HEARTBEAT_INTERVAL))),
        |state| async move {
            let (mut upstream, mut collected, mut audit, mut heartbeat_timer) = state?;

            tokio::select! {
                chunk_result = upstream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            collected.extend_from_slice(&chunk);
                            Some((Ok(chunk), Some((upstream, collected, audit, heartbeat_timer))))
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
                }
                _ = heartbeat_timer.tick() => {
                    Some((
                        Ok(HEARTBEAT_CHUNK.into()),
                        Some((upstream, collected, audit, heartbeat_timer)),
                    ))
                }
            }
        },
    );

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

fn websocket_stream_error_allows_http_sse_fallback(error: &CodexClientError) -> bool {
    match error {
        CodexClientError::WebSocket(
            CodexWebSocketError::Transport(_) | CodexWebSocketError::EmptyResponse,
        ) => true,
        CodexClientError::Upstream { status, .. } => matches!(
            *status,
            StatusCode::NOT_FOUND
                | StatusCode::METHOD_NOT_ALLOWED
                | StatusCode::UPGRADE_REQUIRED
                | StatusCode::NOT_IMPLEMENTED
        ),
        _ => false,
    }
}

async fn send_codex_request_with_refresh_retry_deps(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::gateway::transport::http_client::CodexBackendResponse, CodexClientError> {
    let result = match send_codex_request(deps, request, account, request_id).await {
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
    };
    if let Ok(response) = &result {
        apply_rate_limit_headers_with_deps(
            deps,
            &account.id,
            account.plan_type.as_deref(),
            &response.rate_limit_headers,
        )
        .await;
    }
    result
}

async fn send_codex_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::gateway::transport::http_client::CodexBackendResponse, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };

    let account_scope = &account.id;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        account_scope,
    );

    let installation_id = get_installation_id(Some(&deps.config.database.url));

    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),
    )
    .with_websocket_pool(deps.websocket_pool.clone(), account.id.clone());
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
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id: Some(&installation_id),
                session_id: identity.conversation_id.as_deref(),
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

    // Build conversation identity for session affinity
    // Use account.id as the scope since entry_id doesn't exist
    let account_scope = &account.id;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        account_scope,
    );

    // Get installation ID (cached after first call)
    let installation_id = get_installation_id(Some(&deps.config.database.url));

    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),
    )
    .with_websocket_pool(deps.websocket_pool.clone(), account.id.clone());
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
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id: Some(&installation_id),
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn send_codex_websocket_stream_request_with_refresh_retry(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendWebSocketStream, CodexClientError> {
    match send_codex_websocket_stream_request(deps, request, account, request_id).await {
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
            send_codex_websocket_stream_request(deps, request, &refreshed, request_id).await
        }
        result => result,
    }
}

async fn send_codex_websocket_stream_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendWebSocketStream, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };

    let account_scope = &account.id;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        account_scope,
    );
    let installation_id = get_installation_id(Some(&deps.config.database.url));

    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),
    )
    .with_websocket_pool(deps.websocket_pool.clone(), account.id.clone());
    client
        .websocket_stream_response(
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
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id: Some(&installation_id),
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn record_response_affinity_with_deps(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account_id: &str,
    body: &str,
    turn_state: Option<&str>,
    usage: Option<TokenUsage>,
) {
    let Some(conversation_id) = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let metadata = match completed_response_metadata(body) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "解析已完成响应 metadata 用于 session affinity 失败"
            );
            return;
        }
    };

    let variant_hash = compute_variant_hash(request);
    let response_id = metadata.response_id;
    let entry = deps
        .session_affinity
        .record(
            response_id.clone(),
            account_id.to_string(),
            conversation_id.to_string(),
            turn_state
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string)
                .or_else(|| request.turn_state.clone()),
            Some(&request.instructions),
            usage.map(|usage| usage.input_tokens),
            Some(metadata.function_call_ids),
            variant_hash,
        )
        .await;
    if let Some(repository) = deps.session_affinity_repository.as_ref() {
        if let Err(error) = repository
            .upsert(&response_id, &entry, deps.session_affinity.ttl())
            .await
        {
            tracing::warn!(
                error = %error,
                response_id = %response_id,
                account_id = %account_id,
                "持久化 session affinity 失败"
            );
        }
    }
}

async fn responses_websocket_stream(
    deps: CodexUpstreamDependencies,
    request: CodexResponsesRequest,
    mut acquired: AcquiredAccount,
    mut log_context: CodexRequestLogContext,
) -> Response {
    let mut excluded_account_ids = Vec::new();
    let stream_response = loop {
        stagger_request_with_deps(&deps, acquired.previous_slot_at).await;
        let response = send_codex_websocket_stream_request_with_refresh_retry(
            &deps,
            &request,
            &acquired.account,
            log_context.request_id.as_str(),
        )
        .await;

        match response {
            Ok(response) => break response,
            Err(error) => {
                if transport_for_request(&request) == CodexTransport::WebSocketPreferred
                    && websocket_stream_error_allows_http_sse_fallback(&error)
                {
                    acquired.previous_slot_at = None;
                    return responses_http_sse_stream(deps, request, acquired, log_context).await;
                }
                deps.account_pool.lock().await.release(&acquired.account.id);
                if let Some(retry) = classify_upstream_account_retry(&error) {
                    if request.previous_response_id.is_some() {
                        // previous_response_id 的历史由上游账号持有，换账号会静默丢失会话上下文。
                        apply_upstream_account_retry_with_deps(&deps, &acquired.account, retry)
                            .await;
                        log_codex_upstream_response_with_deps(
                            &deps,
                            &log_context,
                            retry.status(),
                            EventLevel::Warn,
                            "v1 responses WebSocket history 请求保持原账户",
                            websocket_history_retry_metadata(retry, true),
                        )
                        .await;
                    } else {
                        let fallback = apply_upstream_retry_and_acquire_fallback_with_deps(
                            &deps,
                            &acquired.account,
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
                            "v1 responses WebSocket 上游请求将使用备用账户重试",
                            retry.metadata(true),
                        )
                        .await;
                        if let Some(fallback) = fallback {
                            log_context = log_context.with_account(&fallback.account.id);
                            acquired = fallback;
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
                    "v1 responses WebSocket stream 上游请求失败",
                    json!({"stream": true, "transport": "websocket"}),
                )
                .await;
                return error_response.into_response();
            }
        }
    };

    if persist_upstream_cookies_with_deps(
        &deps,
        &acquired.account.id,
        &stream_response.set_cookie_headers,
    )
    .await
    .is_err()
    {
        deps.account_pool.lock().await.release(&acquired.account.id);
        log_codex_upstream_response_with_deps(
            &deps,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses WebSocket stream 持久化 cookie 失败",
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

    let audit = WebSocketStreamAudit::new(
        deps,
        log_context,
        acquired.account.id,
        acquired.account.plan_type,
        request,
        stream_response.turn_state,
        stream_response.rate_limit_headers,
        stream_response.rate_limit_updates,
    );
    let upstream = stream_response.body_stream;

    use tokio::time::{interval, Duration};
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
    const HEARTBEAT_CHUNK: &str = ": ping\n\n";

    let body_stream = futures_stream::unfold(
        Some((upstream, Vec::new(), audit, interval(HEARTBEAT_INTERVAL))),
        |state| async move {
            let (mut upstream, mut collected, mut audit, mut heartbeat_timer) = state?;

            tokio::select! {
                chunk_result = upstream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            collected.extend_from_slice(chunk.as_bytes());
                            Some((
                                Ok::<String, std::io::Error>(chunk),
                                Some((upstream, collected, audit, heartbeat_timer)),
                            ))
                        }
                        Some(Err(error)) => {
                            audit.log_transport_error(&error).await;
                            Some((Err(std::io::Error::other(error.to_string())), None))
                        }
                        None => {
                            audit.complete(&collected).await;
                            None
                        }
                    }
                }
                _ = heartbeat_timer.tick() => {
                    Some((
                        Ok::<String, std::io::Error>(HEARTBEAT_CHUNK.to_string()),
                        Some((upstream, collected, audit, heartbeat_timer)),
                    ))
                }
            }
        },
    );

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

async fn log_codex_upstream_response_with_deps(
    deps: &CodexUpstreamDependencies,
    context: &CodexRequestLogContext,
    status: StatusCode,
    level: EventLevel,
    message: &str,
    mut metadata: Value,
) {
    ensure_stream_metadata(&mut metadata, context.stream);
    let mut event = EventLog::new("v1.response", level, message);
    event.request_id = Some(context.request_id.clone());
    event.account_id = Some(context.account_id.clone());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(context.model.clone());
    event.status_code = Some(i64::from(status.as_u16()));
    event.latency_ms = Some(context.latency_ms());
    event.metadata = metadata;
    if let Err(error) = deps.logs.record(event).await {
        tracing::warn!(error = %error, "写入 v1 response 事件日志失败");
    }
}
