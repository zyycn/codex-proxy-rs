//! OpenAI 上游失败分类、恢复决策与稳定错误投影。

use super::*;

pub(super) struct MappedProviderFailure {
    pub(super) error: ProviderError,
    pub(super) websocket_transport_retryable: bool,
    pub(super) account_failure: Option<CodexAccountFailure>,
    /// 原始上游错误描述，仅在凭据错误状态下持久化。
    pub(super) error_message: Option<String>,
    pub(super) cyber_policy_failure: bool,
    pub(super) set_cookie_headers: Vec<String>,
    pub(super) rate_limit_headers: Vec<(String, String)>,
    pub(super) observation: Option<ProviderResponseObservation>,
    pub(super) capture_response_cookies: bool,
}

impl MappedProviderFailure {
    pub(super) fn plain(error: ProviderError) -> Self {
        Self {
            error,
            websocket_transport_retryable: false,
            account_failure: None,
            error_message: None,
            cyber_policy_failure: false,
            set_cookie_headers: Vec::new(),
            rate_limit_headers: Vec::new(),
            observation: None,
            capture_response_cookies: false,
        }
    }
}

pub(super) enum PreCommitPoll<T> {
    Upstream(T),
    GraceElapsed,
}

pub(super) async fn wait_for_replay_grace(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// 提交边界前的上游事件预取。
///
/// 原始 chunk 计数而不是重编码后的 event 大小。时间与 64 KiB 共同限定无感换号
/// 窗口；任一边界到达都会提交已缓存 wire，不能因网关私有资源规则伪造上游协议
/// 失败。一旦提交，后续事件不再具备无痕重放资格。
pub(super) struct PreCommitClientEvents {
    pending: Vec<ProviderEvent>,
    prefetched_bytes: usize,
    replay_grace_deadline: Option<Instant>,
    committed: bool,
}

impl PreCommitClientEvents {
    pub(super) const fn new() -> Self {
        Self {
            pending: Vec::new(),
            prefetched_bytes: 0,
            replay_grace_deadline: None,
            committed: false,
        }
    }

    pub(super) fn observe_chunk(&mut self, bytes: usize) {
        if !self.committed {
            self.prefetched_bytes = self.prefetched_bytes.saturating_add(bytes);
        }
    }

    pub(super) fn stage(
        &mut self,
        incoming: Vec<ProviderEvent>,
        timing_signals: ResponseEventSignals,
        completed: bool,
    ) -> Vec<ProviderEvent> {
        if self.committed {
            return incoming;
        }
        let starts_replay_grace = incoming.iter().any(ProviderEvent::has_client_event);
        self.pending.extend(incoming);
        if timing_signals.semantic_output
            || completed
            || self.prefetched_bytes > MAX_STREAM_PREFETCH_BYTES
        {
            return self.commit_pending();
        }
        if starts_replay_grace && self.replay_grace_deadline.is_none() {
            self.replay_grace_deadline = Instant::now().checked_add(STREAM_REPLAY_GRACE);
        }
        Vec::new()
    }

    pub(super) fn finish(
        &mut self,
        incoming: Vec<ProviderEvent>,
        timing_signals: ResponseEventSignals,
        completed: bool,
    ) -> Vec<ProviderEvent> {
        let events = self.stage(incoming, timing_signals, completed);
        if self.committed {
            events
        } else {
            self.commit_pending()
        }
    }

    pub(super) fn commit(&mut self, incoming: Vec<ProviderEvent>) -> Vec<ProviderEvent> {
        if self.committed {
            return incoming;
        }
        self.pending.extend(incoming);
        self.commit_pending()
    }

    pub(super) fn take_for_failure(&mut self, incoming: Vec<ProviderEvent>) -> Vec<ProviderEvent> {
        self.pending.extend(incoming);
        self.prefetched_bytes = 0;
        self.replay_grace_deadline = None;
        std::mem::take(&mut self.pending)
    }

    pub(super) const fn replay_grace_deadline(&self) -> Option<Instant> {
        self.replay_grace_deadline
    }

    pub(super) const fn is_committed(&self) -> bool {
        self.committed
    }

    pub(super) fn commit_pending(&mut self) -> Vec<ProviderEvent> {
        self.committed = true;
        self.prefetched_bytes = 0;
        self.replay_grace_deadline = None;
        std::mem::take(&mut self.pending)
    }
}

#[derive(Clone, Copy)]
pub(super) enum ReplayBoundary {
    BeforeSemanticOutput,
    AfterSemanticOutput,
}

impl ReplayBoundary {
    pub(super) const fn from_semantic_output(semantic_output_seen: bool) -> Self {
        if semantic_output_seen {
            Self::AfterSemanticOutput
        } else {
            Self::BeforeSemanticOutput
        }
    }

    pub(super) const fn permits_provider_proof(self) -> bool {
        matches!(self, Self::BeforeSemanticOutput)
    }
}

pub(super) struct OpenAiFailureContext<'a> {
    pub(super) client: &'a CodexBackendClient,
    pub(super) selector: &'a CodexCredentialSelector,
    pub(super) quota: &'a Arc<CodexCredentialQuotaService>,
    pub(super) response_origin: &'a Url,
    pub(super) cyber_policy_scope: Option<&'a CodexCyberPolicyScope>,
    pub(super) allows_account_state_mutation: bool,
}

#[derive(Clone, Copy)]
pub(super) struct UpstreamErrorLogContext<'a> {
    request_id: &'a str,
    account_id: &'a str,
    attempt_index: u32,
    websocket_connection_id: Option<Uuid>,
}

impl<'a> UpstreamErrorLogContext<'a> {
    pub(super) fn new(
        context: &'a AttemptContext,
        account: &'a ProviderAccount,
        websocket_connection_id: Option<Uuid>,
    ) -> Self {
        Self {
            request_id: context.request_id().as_str(),
            account_id: account.id().as_str(),
            attempt_index: context.attempt_index().get(),
            websocket_connection_id,
        }
    }

    fn with_websocket_connection_id(mut self, connection_id: Option<Uuid>) -> Self {
        self.websocket_connection_id = self.websocket_connection_id.or(connection_id);
        self
    }
}

pub(super) fn log_client_upstream_error(
    context: UpstreamErrorLogContext<'_>,
    error: &CodexClientError,
) {
    match error {
        CodexClientError::Upstream {
            status,
            body,
            transport,
            ..
        } => log_raw_upstream_body(
            context,
            *transport,
            "http_error_response",
            Some(status.as_u16()),
            None,
            None,
            body,
        ),
        CodexClientError::WebSocket(CodexWebSocketExchangeError::Upstream(upstream)) => {
            log_raw_upstream_body(
                context,
                CodexBackendTransport::WebSocket,
                "websocket_opening_response",
                Some(upstream.status_code),
                None,
                None,
                &upstream.body,
            );
        }
        CodexClientError::WebSocket(error) => {
            let Some(close) = error.close_before_terminal() else {
                return;
            };
            let context = context.with_websocket_connection_id(close.connection_id());
            let websocket_connection_id = context
                .websocket_connection_id
                .map(|connection_id| connection_id.to_string())
                .unwrap_or_default();
            let close_reason_bytes = close.reason().map_or(0, str::len);
            tracing::warn!(
                request_id = %context.request_id,
                account_id = %context.account_id,
                attempt_index = context.attempt_index,
                websocket_connection_id,
                upstream_transport = WEBSOCKET_TRANSPORT,
                upstream_error_kind = "websocket_close",
                upstream_close_code = close.code().unwrap_or_default(),
                upstream_close_code_present = close.code().is_some(),
                upstream_last_event_type = close.last_event_type().unwrap_or_default(),
                upstream_last_event_type_present = close.last_event_type().is_some(),
                upstream_terminal_seen = false,
                upstream_close_reason_bytes = close_reason_bytes,
                upstream_close_reason_present = close.reason().is_some(),
                upstream_error_raw_present = close.reason().is_some(),
                "OpenAI upstream WebSocket closed before terminal event"
            );
        }
        _ => {}
    }
}

pub(super) fn log_canonical_upstream_error(
    context: UpstreamErrorLogContext<'_>,
    transport: CodexBackendTransport,
    error: &CodexCanonicalError,
) {
    let CodexCanonicalError::Upstream(failure) = error else {
        return;
    };
    log_raw_upstream_body(
        context,
        transport,
        "responses_error_event",
        failure.explicit_status_code,
        failure.upstream_code.as_deref(),
        failure.upstream_type.as_deref(),
        failure.raw_body(),
    );
}

pub(super) fn log_raw_upstream_body(
    context: UpstreamErrorLogContext<'_>,
    transport: CodexBackendTransport,
    error_kind: &'static str,
    status_code: Option<u16>,
    upstream_code: Option<&str>,
    upstream_type: Option<&str>,
    upstream_error_raw: &str,
) {
    let websocket_connection_id = context
        .websocket_connection_id
        .map(|connection_id| connection_id.to_string())
        .unwrap_or_default();
    tracing::warn!(
        request_id = %context.request_id,
        account_id = %context.account_id,
        attempt_index = context.attempt_index,
        websocket_connection_id,
        upstream_transport = actual_transport_name(transport),
        upstream_error_kind = error_kind,
        upstream_status_code = status_code.unwrap_or_default(),
        upstream_status_code_present = status_code.is_some(),
        upstream_error_code = upstream_code.unwrap_or_default(),
        upstream_error_code_present = upstream_code.is_some(),
        upstream_error_type = upstream_type.unwrap_or_default(),
        upstream_error_type_present = upstream_type.is_some(),
        upstream_error_raw,
        upstream_error_raw_bytes = upstream_error_raw.len(),
        "OpenAI upstream returned an error payload"
    );
}

pub(super) async fn apply_failure(
    context: &OpenAiFailureContext<'_>,
    account: &ProviderAccount,
    failure: &MappedProviderFailure,
) {
    if !context.allows_account_state_mutation {
        return;
    }
    synchronize_passive_quota_headers(context.quota, account, &failure.rate_limit_headers).await;
    let needs_authoritative_quota_refresh = matches!(
        failure.account_failure,
        Some(CodexAccountFailure::QuotaExhausted | CodexAccountFailure::UsageLimitExhausted { .. })
    );
    if failure.cyber_policy_failure {
        context
            .selector
            .record_cyber_policy_failure(context.cyber_policy_scope, account)
            .await;
    }
    if let Some(account_failure) = failure.account_failure {
        context
            .client
            .evict_websocket_account(account.id().as_str())
            .await;
        if let Err(error) = context
            .selector
            .record_failure(account, account_failure, failure.error_message.clone())
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "Failed to persist OpenAI account failure state"
            );
        }
    }
    if needs_authoritative_quota_refresh {
        schedule_authoritative_quota_refresh_after_failure(context.quota, account);
    }
    if failure.capture_response_cookies
        && !failure.set_cookie_headers.is_empty()
        && let Err(error) = context
            .selector
            .capture_response_cookies(
                account,
                context.response_origin,
                &failure.set_cookie_headers,
            )
            .await
    {
        tracing::warn!(
            account_id = %account.id(),
            error = %error,
            "Failed to persist OpenAI response cookies"
        );
    }
}

pub(super) fn schedule_authoritative_quota_refresh_after_failure(
    quota: &Arc<CodexCredentialQuotaService>,
    account: &ProviderAccount,
) {
    // 限额错误可以确认账号状态，但 Responses 事件中的 used_percent 可能仍停在上一结算点。
    // usage 快照在后台补齐展示基线，不得撤销同一轮真实失败，也不能阻塞原始响应。
    // 先等待 2 秒让上游结算，再查询；5 秒仅限制实际查询自身。
    let quota = Arc::clone(quota);
    let account_id = account.id().clone();
    drop(tokio::spawn(async move {
        tokio::time::sleep(QUOTA_FAILURE_REFRESH_DELAY).await;
        match tokio::time::timeout(
            QUOTA_FAILURE_REFRESH_TIMEOUT,
            quota.refresh_account_after_failure(&account_id),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    account_id = %account_id,
                    error = %error,
                    "OpenAI authoritative quota synchronization after quota rejection failed"
                );
            }
            Err(_) => {
                tracing::warn!(
                    account_id = %account_id,
                    timeout_ms = QUOTA_FAILURE_REFRESH_TIMEOUT.as_millis(),
                    "OpenAI authoritative quota synchronization after quota rejection timed out"
                );
            }
        }
    }));
}

pub(super) fn map_handshake_error(error: CodexClientError) -> MappedProviderFailure {
    map_client_error(error, UpstreamSendState::Ambiguous, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WebSocketFailurePolicy {
    Budgeted,
    ImmediateFallback,
}

pub(super) struct WebSocketRecoveryContext<'a> {
    pub(super) policy: WebSocketFailurePolicy,
    pub(super) requirement: TransportRequirement,
    pub(super) retry_count: u32,
    pub(super) max_retries: u32,
    pub(super) request_id: &'a str,
    pub(super) attempt_index: u32,
    pub(super) account_id: &'a str,
    pub(super) session_affinity_key: Option<&'a ProviderSessionAffinityKey>,
    pub(super) session_affinity_key_hash: Option<&'a str>,
    pub(super) session_transport_fallbacks: &'a CodexSessionTransportFallbacks,
}

pub(super) fn websocket_client_failure_policy(
    error: &CodexClientError,
) -> Option<WebSocketFailurePolicy> {
    match error {
        CodexClientError::Upstream {
            status,
            transport: CodexBackendTransport::WebSocket,
            ..
        } if *status == reqwest::StatusCode::UPGRADE_REQUIRED => {
            Some(WebSocketFailurePolicy::ImmediateFallback)
        }
        CodexClientError::Upstream {
            status,
            transport: CodexBackendTransport::WebSocket,
            ..
        } if status.is_server_error()
            || matches!(
                *status,
                reqwest::StatusCode::REQUEST_TIMEOUT
                    | reqwest::StatusCode::CONFLICT
                    | reqwest::StatusCode::TOO_EARLY
                    | reqwest::StatusCode::TOO_MANY_REQUESTS
            ) =>
        {
            Some(WebSocketFailurePolicy::Budgeted)
        }
        CodexClientError::WebSocket(error)
            if !matches!(error, CodexWebSocketExchangeError::InvalidRequest(_)) =>
        {
            Some(WebSocketFailurePolicy::Budgeted)
        }
        _ => None,
    }
}

pub(super) fn apply_websocket_recovery_policy(
    failure: &mut MappedProviderFailure,
    context: WebSocketRecoveryContext<'_>,
) {
    let send_state = failure.error.send_state();
    if matches!(
        send_state,
        UpstreamSendState::Sent | UpstreamSendState::Ambiguous
    ) {
        // payload 已经发送或发送结果不确定时，代理不能用同一业务输入自动重放。
        // 只为客户端下一次完整请求记住 HTTP 偏好，当前请求保持终态失败。
        let sticky_state_transition = context
            .session_affinity_key
            .map(|key| context.session_transport_fallbacks.disable_websocket(key));
        tracing::warn!(
            request_id = context.request_id,
            attempt_index = context.attempt_index,
            account_id = context.account_id,
            websocket_failure_kind = failure.error.kind().as_str(),
            websocket_failure_code = failure
                .error
                .upstream_code()
                .map_or("", OpaqueUpstreamValue::as_str),
            upstream_send_state = ?send_state,
            transport_requirement = context.requirement.as_str(),
            continuation_recovery_action = "client_replay_required",
            session_affinity_present = context.session_affinity_key.is_some(),
            session_affinity_key_hash = context.session_affinity_key_hash.unwrap_or(""),
            sticky_http_enabled = sticky_state_transition.is_some(),
            sticky_state_transition = sticky_state_transition.unwrap_or(false),
            "OpenAI upstream WebSocket failed after payload send; proxy replay was suppressed"
        );
        return;
    }

    let recovery_is_safe = context.requirement.allows_pre_send_http_fallback();
    if !recovery_is_safe {
        return;
    }

    // 传输恢复必须保持原账号可调度；最终 HTTP attempt 若仍失败，再按真实 HTTP
    // 结果更新账号健康度，避免中间 WS 错误把同账号钉选提前冷却掉。
    failure.account_failure = None;
    let fallback_now = context.policy == WebSocketFailurePolicy::ImmediateFallback
        || context.retry_count >= context.max_retries;
    if !fallback_now {
        let Some(retry_index) = context.retry_count.checked_add(1).and_then(NonZeroU32::new) else {
            return;
        };
        let delay = failure
            .error
            .retry_after()
            .unwrap_or_else(|| websocket_retry_backoff(retry_index));
        failure
            .error
            .set_pre_delivery_transport_retry(retry_index, delay);
        tracing::warn!(
            request_id = context.request_id,
            attempt_index = context.attempt_index,
            account_id = context.account_id,
            websocket_failure_kind = failure.error.kind().as_str(),
            websocket_failure_code = failure
                .error
                .upstream_code()
                .map_or("", OpaqueUpstreamValue::as_str),
            upstream_status_code = failure.error.upstream_status().unwrap_or_default(),
            upstream_status_code_present = failure.error.upstream_status().is_some(),
            websocket_retry_count = retry_index.get(),
            websocket_max_retries = context.max_retries,
            websocket_retry_delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            transport_requirement = context.requirement.as_str(),
            "OpenAI upstream WebSocket failed; retrying the same account"
        );
        return;
    }

    failure.error.set_pre_delivery_transport_fallback();
    let sticky_state_transition = context
        .session_affinity_key
        .map(|key| context.session_transport_fallbacks.disable_websocket(key));
    tracing::warn!(
        request_id = context.request_id,
        attempt_index = context.attempt_index,
        account_id = context.account_id,
        websocket_failure_kind = failure.error.kind().as_str(),
        websocket_failure_code = failure
            .error
            .upstream_code()
            .map_or("", OpaqueUpstreamValue::as_str),
        upstream_status_code = failure.error.upstream_status().unwrap_or_default(),
        upstream_status_code_present = failure.error.upstream_status().is_some(),
        websocket_retry_count = context.retry_count,
        websocket_max_retries = context.max_retries,
        websocket_fallback_reason = match context.policy {
            WebSocketFailurePolicy::Budgeted => "retry_budget_exhausted",
            WebSocketFailurePolicy::ImmediateFallback => "upgrade_required",
        },
        session_affinity_present = context.session_affinity_key.is_some(),
        session_affinity_key_hash = context.session_affinity_key_hash.unwrap_or(""),
        sticky_http_enabled = sticky_state_transition.is_some(),
        sticky_state_transition = sticky_state_transition.unwrap_or(false),
        "OpenAI upstream WebSocket disabled for the current session"
    );
}

pub(super) fn websocket_retry_backoff(retry_index: NonZeroU32) -> Duration {
    const INITIAL_DELAY_MS: u64 = 200;

    let shift = retry_index.get().saturating_sub(1).min(63);
    let base_ms = INITIAL_DELAY_MS.saturating_mul(1_u64.checked_shl(shift).unwrap_or(u64::MAX));
    let mut random = [0_u8; 2];
    let jitter_per_mille = if getrandom::fill(&mut random).is_ok() {
        900_u64 + u64::from(u16::from_le_bytes(random) % 200)
    } else {
        1_000
    };
    Duration::from_millis(base_ms.saturating_mul(jitter_per_mille) / 1_000)
}

pub(super) fn continuation_replay_required_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::NotSent,
    )
    .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
    .with_continuation_recovery_disposition(
        ContinuationRecoveryDisposition::ClientReplayRequired,
    )
    .with_upstream_code(OpaqueUpstreamValue::new(
        PREVIOUS_RESPONSE_NOT_FOUND_CODE.to_owned(),
    ))
    .with_diagnostic(ProviderDiagnostic::new(
        "previous response is unavailable in the selected upstream scope; client replay is required",
    ))
    .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        PREVIOUS_RESPONSE_NOT_FOUND_MESSAGE,
        Some(PREVIOUS_RESPONSE_NOT_FOUND_CODE.to_owned()),
        Some("invalid_request_error".to_owned()),
    ))
}

pub(super) fn map_stream_error(error: CodexClientError) -> MappedProviderFailure {
    let allows_pre_delivery_retry = stream_transport_allows_pre_delivery_retry(&error);
    let websocket_failure = error.transport() == Some(CodexBackendTransport::WebSocket);
    let mut failure = map_client_error(error, UpstreamSendState::Sent, false);
    failure.websocket_transport_retryable = allows_pre_delivery_retry && websocket_failure;
    if allows_pre_delivery_retry && !websocket_failure {
        failure.error = failure.error.with_pre_delivery_retry();
    }
    failure
}

pub(super) fn stream_transport_allows_pre_delivery_retry(error: &CodexClientError) -> bool {
    match error {
        CodexClientError::Http(_)
        | CodexClientError::HttpJson(_)
        | CodexClientError::StreamIdleTimeout { .. } => true,
        CodexClientError::WebSocket(error) => !matches!(
            error,
            CodexWebSocketExchangeError::InvalidRequest(_)
                | CodexWebSocketExchangeError::Upstream(_)
                | CodexWebSocketExchangeError::ContinuationUnavailable { .. }
        ),
        _ => false,
    }
}

pub(super) fn map_canonical_error(
    error: CodexCanonicalError,
    diagnostics: &CodexUpstreamDiagnostics,
    set_cookie_headers: &[String],
    rate_limit_headers: &[(String, String)],
    replay_boundary: ReplayBoundary,
) -> MappedProviderFailure {
    match error {
        CodexCanonicalError::Protocol(error) => MappedProviderFailure::plain(error),
        CodexCanonicalError::Upstream(failure) => map_upstream_failure(
            CodexUpstreamFailure::from_sse_failure(
                &failure,
                diagnostics,
                set_cookie_headers,
                rate_limit_headers,
                CodexUpstreamSendPhase::AfterPayload,
            ),
            None,
            replay_boundary,
        ),
    }
}

pub(super) fn map_client_error(
    error: CodexClientError,
    uncertain_state: UpstreamSendState,
    observe_transport: bool,
) -> MappedProviderFailure {
    let websocket_diagnostic = match &error {
        CodexClientError::WebSocket(error) => Some(websocket_diagnostic(error)),
        _ => None,
    };
    let raw_upstream_error = match &error {
        CodexClientError::WebSocket(error) => websocket_raw_error(error),
        _ => None,
    };
    let continuation_failure = match &error {
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason: PreviousResponseUnavailableReason::ConnectionBusy,
        }) => Some(ContinuationFailure::Busy),
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            ..
        }) => Some(ContinuationFailure::HistoryUnavailable),
        _ => None,
    };
    let observation = observe_transport
        .then(|| codex_error_observation(&error))
        .flatten();
    if let Some(failure) = error.upstream_failure() {
        return map_upstream_failure(failure, observation, ReplayBoundary::BeforeSemanticOutput);
    }
    let mut failure = match error {
        CodexClientError::Upstream { .. } => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        )),
        CodexClientError::InvalidHeaderName(_)
        | CodexClientError::InvalidHeaderValue(_)
        | CodexClientError::WebSocketEncode(_)
        | CodexClientError::RequestBodyEncode(_)
        | CodexClientError::RequestCompression(_)
        | CodexClientError::ModelCatalog(_)
        | CodexClientError::CustomCa(_) => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::NotSent,
        )),
        CodexClientError::StreamIdleTimeout { .. } => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        )),
        CodexClientError::InvalidSse(_) => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        )),
        CodexClientError::Http(error) | CodexClientError::HttpJson(error) => {
            let send_state = if error.is_connect() {
                UpstreamSendState::NotSent
            } else {
                uncertain_state
            };
            MappedProviderFailure::plain(provider_error(
                if error.is_timeout() {
                    ProviderErrorKind::Timeout
                } else {
                    ProviderErrorKind::Transport
                },
                send_state,
            ))
        }
        CodexClientError::WebSocket(
            error @ CodexWebSocketExchangeError::ContinuationUnavailable { .. },
        ) => {
            let mut failure = MappedProviderFailure::plain(continuation_replay_required_error());
            if let Some(client_visible_error) = websocket_client_visible_error(&error) {
                failure.error = failure
                    .error
                    .with_client_visible_upstream_error(client_visible_error);
            }
            failure
        }
        CodexClientError::WebSocket(error) => {
            let close_code = error.close_before_terminal().and_then(|close| close.code());
            let client_visible_error = websocket_client_visible_error(&error);
            let mut failure = MappedProviderFailure::plain(provider_error(
                websocket_error_kind(&error),
                websocket_send_state(&error),
            ));
            if let Some(close_code) = close_code {
                failure.error =
                    failure
                        .error
                        .with_upstream_code(OpaqueUpstreamValue::new(format!(
                            "websocket_close_{close_code}"
                        )));
            }
            if matches!(error, CodexWebSocketExchangeError::ConnectionLimitReached) {
                failure.error = failure.error.with_upstream_code(OpaqueUpstreamValue::new(
                    WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE.to_owned(),
                ));
            }
            if let Some(client_visible_error) = client_visible_error {
                failure.error = failure
                    .error
                    .with_client_visible_upstream_error(client_visible_error);
            }
            failure
        }
    };
    if let Some(continuation_failure) = continuation_failure {
        failure.error = failure
            .error
            .with_continuation_failure(continuation_failure)
            .with_continuation_recovery_disposition(
                ContinuationRecoveryDisposition::ClientReplayRequired,
            );
    }
    if let Some(diagnostic) = websocket_diagnostic {
        failure.error = failure.error.with_diagnostic(diagnostic);
    }
    if let Some(raw) = raw_upstream_error {
        failure.error = failure.error.with_raw_upstream_error(raw);
    }
    failure.observation = observation;
    failure
}

fn websocket_diagnostic(error: &CodexWebSocketExchangeError) -> ProviderDiagnostic {
    if let Some(close) = error.close_before_terminal() {
        let mut message = close.code().map_or_else(
            || "OpenAI WebSocket closed before a terminal response".to_owned(),
            |code| {
                format!("OpenAI WebSocket closed before a terminal response (close code {code})")
            },
        );
        if let Some(last_event_type) = close.last_event_type() {
            message.push_str("; last event type: ");
            message.push_str(last_event_type);
        }
        return ProviderDiagnostic::new(message);
    }

    ProviderDiagnostic::new(match error {
        CodexWebSocketExchangeError::InvalidRequest(_) => {
            "OpenAI WebSocket request could not be constructed".to_owned()
        }
        CodexWebSocketExchangeError::Transport(_)
        | CodexWebSocketExchangeError::PostSendAmbiguous { .. } => {
            "OpenAI WebSocket transport failed after payload send; result is ambiguous".to_owned()
        }
        CodexWebSocketExchangeError::Connect(_) => {
            "OpenAI WebSocket connection failed before payload send".to_owned()
        }
        CodexWebSocketExchangeError::ConnectTimeout { timeout } => {
            format!("OpenAI WebSocket connection timed out after {timeout:?}")
        }
        CodexWebSocketExchangeError::FastPathTimeout { timeout } => {
            format!("OpenAI WebSocket fast-path budget expired after {timeout:?}")
        }
        CodexWebSocketExchangeError::OriginCircuitOpen => {
            "OpenAI WebSocket origin circuit is open".to_owned()
        }
        CodexWebSocketExchangeError::OriginHalfOpenBusy => {
            "OpenAI WebSocket origin half-open probe is busy".to_owned()
        }
        CodexWebSocketExchangeError::SharedConnectFailed => {
            "OpenAI shared WebSocket connection failed before payload send".to_owned()
        }
        CodexWebSocketExchangeError::SendTimeout { timeout } => {
            format!("OpenAI WebSocket payload send timed out after {timeout:?}")
        }
        CodexWebSocketExchangeError::InvalidSse(_) => {
            "OpenAI WebSocket returned an invalid Responses event stream".to_owned()
        }
        CodexWebSocketExchangeError::Upstream(upstream) => format!(
            "OpenAI WebSocket opening was rejected with status {}",
            upstream.status_code
        ),
        CodexWebSocketExchangeError::ConnectionLimitReached => {
            "OpenAI requested a new WebSocket connection".to_owned()
        }
        CodexWebSocketExchangeError::ContinuationUnavailable { reason } => {
            format!("OpenAI connection-local previous response is unavailable: {reason}")
        }
        CodexWebSocketExchangeError::ReceiveIdleTimeout { timeout } => {
            format!("OpenAI WebSocket receive idle timeout after {timeout:?}")
        }
        CodexWebSocketExchangeError::UnexpectedBinaryEvent => {
            "OpenAI WebSocket returned an unexpected binary event".to_owned()
        }
        CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent { .. } => {
            "Reused OpenAI WebSocket died before the first upstream event".to_owned()
        }
        CodexWebSocketExchangeError::InitialEventTimeout { timeout } => {
            format!("OpenAI WebSocket first-event timeout after {timeout:?}")
        }
        CodexWebSocketExchangeError::ClosedBeforeTerminal(_) => {
            unreachable!("close-before-terminal errors are handled before the variant match")
        }
    })
}

fn websocket_raw_error(error: &CodexWebSocketExchangeError) -> Option<RawUpstreamError> {
    let close = error.close_before_terminal()?;
    Some(RawUpstreamError::new(
        json!({
            "type": "websocket.close",
            "code": close.code(),
            "reason": close.reason(),
            "last_event_type": close.last_event_type(),
        })
        .to_string(),
    ))
}

pub(super) fn websocket_client_visible_error(
    error: &CodexWebSocketExchangeError,
) -> Option<ClientVisibleUpstreamError> {
    if matches!(error, CodexWebSocketExchangeError::ConnectionLimitReached) {
        return Some(ClientVisibleUpstreamError::new(
            "websocket connection limit reached",
            Some(WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE.to_owned()),
            Some("websocket_error".to_owned()),
        ));
    }
    let close = error.close_before_terminal()?;
    let message = close
        .reason()
        .filter(|reason| !reason.is_empty())
        .map_or_else(|| close.to_string(), str::to_owned);
    Some(ClientVisibleUpstreamError::new(
        message,
        close.code().map(|code| code.to_string()),
        Some("websocket_close_error".to_owned()),
    ))
}

pub(super) fn map_upstream_failure(
    mut failure: CodexUpstreamFailure,
    observation: Option<ProviderResponseObservation>,
    replay_boundary: ReplayBoundary,
) -> MappedProviderFailure {
    let category = failure.category();
    let cyber_policy_failure = failure
        .status
        .is_some_and(|status| status.is_client_error())
        && is_cyber_policy_code(failure.code.as_deref());
    let continuation_failure = failure
        .persistable_code()
        .filter(|code| is_history_failure_code(code))
        .map(|_| ContinuationFailure::HistoryUnavailable);
    let send_state = upstream_send_state(failure.send_phase);
    let mut error = provider_error(provider_error_kind(category), send_state);
    error = error.with_raw_upstream_error(RawUpstreamError::new(failure.raw_body.clone()));
    if let Some(message) = failure.client_message.as_ref() {
        error = error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            message.clone(),
            failure.client_code.clone(),
            failure.client_error_type.clone(),
        ));
    }
    if let Some(response) = failure.client_response.take() {
        let response = (*response).into_parts();
        error = error.with_client_visible_upstream_response(
            ClientVisibleUpstreamResponse::new(
                response.status,
                response.content_type,
                response.body,
            )
            .with_headers(
                response
                    .client_headers
                    .into_iter()
                    .map(|(name, value)| ProviderResponseHeader::new(name, value))
                    .collect(),
            ),
        );
    }
    if let Some(status) = failure.status {
        error = error.with_status(status.as_u16());
    }
    if replay_boundary.permits_provider_proof()
        && (failure.replay_is_safe() || cyber_policy_failure)
    {
        error = error.with_replay_safe();
    }
    if let Some(continuation_failure) = continuation_failure {
        error = error
            .with_continuation_failure(continuation_failure)
            .with_continuation_recovery_disposition(
                ContinuationRecoveryDisposition::ClientReplayRequired,
            );
    }
    if let Some(retry_after) = failure.retry_after_seconds.map(Duration::from_secs) {
        error = error.with_retry_after(retry_after);
    }
    if let Some(code) = failure.persistable_code() {
        error = error.with_upstream_code(OpaqueUpstreamValue::new(code.to_owned()));
    }
    if let Some(request_id) = failure.request_id.as_deref() {
        error = error.with_upstream_request_id(OpaqueUpstreamValue::new(request_id.to_owned()));
    }
    let status = error
        .upstream_status()
        .map_or_else(|| "none".to_owned(), |status| status.to_string());
    let code = error
        .upstream_code()
        .map_or_else(|| "none".to_owned(), |code| code.as_str().to_owned());
    let kind = error.kind().as_str();
    error = error.with_diagnostic(ProviderDiagnostic::new(format!(
        "OpenAI upstream failure: kind={}, status={status}, code={code}",
        kind
    )));
    MappedProviderFailure {
        error,
        websocket_transport_retryable: false,
        account_failure: account_failure(
            category,
            failure.retry_after_seconds,
            failure.usage_limit_resets_at,
        ),
        error_message: failure.client_message,
        cyber_policy_failure,
        set_cookie_headers: failure.set_cookie_headers,
        rate_limit_headers: failure.rate_limit_headers,
        observation,
        capture_response_cookies: !matches!(
            category,
            CodexFailureCategory::CloudflareChallenge | CodexFailureCategory::CloudflarePathBlocked
        ),
    }
}

pub(super) fn is_cyber_policy_code(code: Option<&str>) -> bool {
    code.is_some_and(|code| code.trim().eq_ignore_ascii_case("cyber_policy"))
}

pub(super) fn is_history_failure_code(code: &str) -> bool {
    matches!(
        code,
        "previous_response_not_found"
            | "invalid_encrypted_content"
            | "missing_tool_output"
            | "no_tool_output"
    )
}

pub(super) const fn provider_error_kind(category: CodexFailureCategory) -> ProviderErrorKind {
    match category {
        CodexFailureCategory::ModelUnsupported => ProviderErrorKind::Unsupported,
        CodexFailureCategory::CredentialExpired => ProviderErrorKind::Unauthorized,
        CodexFailureCategory::IdentityVerificationRequired | CodexFailureCategory::Banned => {
            ProviderErrorKind::PermissionDenied
        }
        CodexFailureCategory::UsageLimitExhausted => ProviderErrorKind::QuotaExhausted,
        CodexFailureCategory::RateLimited => ProviderErrorKind::RateLimited,
        CodexFailureCategory::QuotaExhausted => ProviderErrorKind::QuotaExhausted,
        CodexFailureCategory::CloudflareChallenge
        | CodexFailureCategory::CloudflarePathBlocked
        | CodexFailureCategory::Unavailable => ProviderErrorKind::Unavailable,
        CodexFailureCategory::InvalidRequest => ProviderErrorKind::InvalidRequest,
        CodexFailureCategory::PermissionDenied => ProviderErrorKind::PermissionDenied,
        CodexFailureCategory::Timeout => ProviderErrorKind::Timeout,
        CodexFailureCategory::Transport => ProviderErrorKind::Transport,
    }
}

pub(super) const fn upstream_send_state(phase: CodexUpstreamSendPhase) -> UpstreamSendState {
    match phase {
        CodexUpstreamSendPhase::BeforePayload => UpstreamSendState::NotSent,
        CodexUpstreamSendPhase::AfterPayload => UpstreamSendState::Sent,
        CodexUpstreamSendPhase::Ambiguous => UpstreamSendState::Ambiguous,
    }
}

pub(super) fn account_failure(
    category: CodexFailureCategory,
    retry_after_seconds: Option<u64>,
    usage_limit_resets_at: Option<i64>,
) -> Option<CodexAccountFailure> {
    match category {
        CodexFailureCategory::CredentialExpired => Some(CodexAccountFailure::CredentialExpired),
        CodexFailureCategory::IdentityVerificationRequired => {
            Some(CodexAccountFailure::IdentityVerificationRequired)
        }
        CodexFailureCategory::Banned => Some(CodexAccountFailure::Banned),
        CodexFailureCategory::UsageLimitExhausted => {
            Some(CodexAccountFailure::UsageLimitExhausted {
                reset_at: usage_limit_resets_at
                    .and_then(|seconds| u64::try_from(seconds).ok())
                    .and_then(|seconds| {
                        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
                    }),
            })
        }
        CodexFailureCategory::RateLimited => Some(CodexAccountFailure::RateLimited {
            retry_after: retry_after_seconds.map(Duration::from_secs),
        }),
        CodexFailureCategory::QuotaExhausted => Some(CodexAccountFailure::QuotaExhausted),
        CodexFailureCategory::CloudflareChallenge => {
            Some(CodexAccountFailure::CloudflareChallenge {
                retry_after: retry_after_seconds.map(Duration::from_secs),
            })
        }
        CodexFailureCategory::CloudflarePathBlocked => {
            Some(CodexAccountFailure::CloudflarePathBlocked)
        }
        CodexFailureCategory::ModelUnsupported
        | CodexFailureCategory::InvalidRequest
        | CodexFailureCategory::PermissionDenied
        | CodexFailureCategory::Timeout
        | CodexFailureCategory::Unavailable
        | CodexFailureCategory::Transport => None,
    }
}

pub(super) const fn websocket_send_state(error: &CodexWebSocketExchangeError) -> UpstreamSendState {
    match error {
        CodexWebSocketExchangeError::InvalidRequest(_)
        | CodexWebSocketExchangeError::Connect(_)
        | CodexWebSocketExchangeError::ConnectTimeout { .. }
        | CodexWebSocketExchangeError::FastPathTimeout { .. }
        | CodexWebSocketExchangeError::OriginCircuitOpen
        | CodexWebSocketExchangeError::OriginHalfOpenBusy
        | CodexWebSocketExchangeError::SharedConnectFailed
        | CodexWebSocketExchangeError::ContinuationUnavailable { .. } => UpstreamSendState::NotSent,
        CodexWebSocketExchangeError::Upstream(_)
        | CodexWebSocketExchangeError::ConnectionLimitReached
        | CodexWebSocketExchangeError::InvalidSse(_)
        | CodexWebSocketExchangeError::UnexpectedBinaryEvent => UpstreamSendState::Sent,
        CodexWebSocketExchangeError::Transport(_)
        | CodexWebSocketExchangeError::PostSendAmbiguous { .. }
        | CodexWebSocketExchangeError::SendTimeout { .. }
        | CodexWebSocketExchangeError::ClosedBeforeTerminal(_)
        | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
        | CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent { .. }
        | CodexWebSocketExchangeError::InitialEventTimeout { .. } => UpstreamSendState::Ambiguous,
    }
}

pub(super) const fn websocket_error_kind(error: &CodexWebSocketExchangeError) -> ProviderErrorKind {
    match error {
        CodexWebSocketExchangeError::InvalidRequest(_)
        | CodexWebSocketExchangeError::InvalidSse(_)
        | CodexWebSocketExchangeError::UnexpectedBinaryEvent => ProviderErrorKind::Protocol,
        CodexWebSocketExchangeError::ConnectTimeout { .. }
        | CodexWebSocketExchangeError::FastPathTimeout { .. }
        | CodexWebSocketExchangeError::SendTimeout { .. }
        | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
        | CodexWebSocketExchangeError::InitialEventTimeout { .. } => ProviderErrorKind::Timeout,
        CodexWebSocketExchangeError::OriginCircuitOpen
        | CodexWebSocketExchangeError::OriginHalfOpenBusy
        | CodexWebSocketExchangeError::SharedConnectFailed
        | CodexWebSocketExchangeError::ContinuationUnavailable { .. } => {
            ProviderErrorKind::Unavailable
        }
        CodexWebSocketExchangeError::Upstream(_) => ProviderErrorKind::Unavailable,
        CodexWebSocketExchangeError::ConnectionLimitReached => ProviderErrorKind::RateLimited,
        CodexWebSocketExchangeError::Transport(_)
        | CodexWebSocketExchangeError::Connect(_)
        | CodexWebSocketExchangeError::PostSendAmbiguous { .. }
        | CodexWebSocketExchangeError::ClosedBeforeTerminal(_)
        | CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent { .. } => {
            ProviderErrorKind::Transport
        }
    }
}

pub(super) fn provider_error(
    kind: ProviderErrorKind,
    send_state: UpstreamSendState,
) -> ProviderError {
    ProviderError::new(kind, send_state)
}

pub(super) fn remaining(deadline: SystemTime) -> Option<Duration> {
    deadline
        .duration_since(SystemTime::now())
        .ok()
        .filter(|remaining| !remaining.is_zero())
}
