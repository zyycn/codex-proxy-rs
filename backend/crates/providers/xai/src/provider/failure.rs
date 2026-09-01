//! xAI 上游失败分类、账号反馈与恢复决策。

use super::*;

pub(super) fn is_invalid_encrypted_content_failure(error: &GrokInferenceTransportError) -> bool {
    error.kind() == GrokInferenceTransportErrorKind::InvalidRequest
        && error.status() == Some(400)
        && error.upstream_code().is_some_and(|code| {
            matches!(
                code.as_str(),
                REASONING_DECODE_FAILED_CODE | "invalid_encrypted_content"
            )
        })
}

pub(super) enum InferenceBoundary {
    Response(Result<crate::transport::GrokInferenceResponse, GrokInferenceTransportError>),
    Cancelled,
    Deadline,
}

pub(super) fn xai_error_observation(
    error: &GrokInferenceTransportError,
) -> Result<ProviderResponseObservation, ProviderError> {
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(HTTP_SSE_TRANSPORT)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, error.send_state()))?,
    );
    if let Some(http_version) = error.http_version() {
        observation = observation.with_http_version(http_version);
    }
    if let Some(status_code) = error.status() {
        observation = observation.with_status_code(status_code);
    }
    if let Some(request_id) = error.request_id().cloned() {
        observation = observation.with_request_id(request_id);
    }
    Ok(with_xai_transport_metrics(
        observation,
        error.transport_metrics(),
    ))
}

pub(super) fn xai_response_observation(
    response: &GrokInferenceResponse,
) -> Result<ProviderResponseObservation, ProviderError> {
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(HTTP_SSE_TRANSPORT)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?,
    )
    .with_http_version(response.http_version())
    .with_status_code(response.status_code());
    if let Some(request_id) = response.request_id().cloned() {
        observation = observation.with_request_id(request_id);
    }
    Ok(with_xai_transport_metrics(
        observation,
        response.transport_metrics(),
    ))
}

pub(super) fn with_xai_transport_metrics(
    mut observation: ProviderResponseObservation,
    metrics: GrokInferenceTransportMetrics,
) -> ProviderResponseObservation {
    observation = observation.with_timings(ProviderResponseTimings {
        headers_ms: metrics.headers_ms(),
        ..ProviderResponseTimings::default()
    });
    if let Some(metadata) = xai_transport_metadata(metrics) {
        observation = observation.with_provider_metadata(metadata);
    }
    observation
}

pub(super) fn xai_transport_metadata(
    metrics: GrokInferenceTransportMetrics,
) -> Option<ProviderResponseMetadata> {
    let mut metadata = Map::new();
    if let Some(status) = metrics.client_cache_status() {
        metadata.insert(
            "clientCache".to_owned(),
            Value::String(status.as_str().to_owned()),
        );
    }
    if let Some(dns) = metrics.dns() {
        metadata.insert(
            "dnsSource".to_owned(),
            Value::String(dns.source().as_str().to_owned()),
        );
        metadata.insert("dnsMs".to_owned(), Value::from(dns.duration_ms()));
    }
    if let Some(headers_ms) = metrics.headers_ms() {
        metadata.insert("upstreamHeadersMs".to_owned(), Value::from(headers_ms));
    }
    if metadata.is_empty() {
        return None;
    }
    // 观测对象版本信封：读取端按 schemaVersion 解码，缺失视为 v0。
    metadata.insert("schemaVersion".to_owned(), Value::from(1));
    ProviderResponseMetadata::new(serde_json::to_string(&Value::Object(metadata)).ok()?)
}

pub(super) async fn record_credential_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: ProviderError,
    failure: GrokCredentialFailure,
) -> ProviderError {
    if session.allows_account_state_mutation() {
        selector.record_failure(session, failure).await;
    }
    error
}

pub(super) async fn record_stream_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: ProviderError,
    upstream_model: &UpstreamModelId,
) -> ProviderError {
    if !session.allows_account_state_mutation() {
        return error;
    }
    if let Some(failure) = stream_credential_failure(&error, upstream_model) {
        return record_credential_failure(selector, session, error, failure).await;
    }
    if error.kind() != ProviderErrorKind::Cancelled {
        selector
            .record_failure(session, GrokCredentialFailure::StreamInterrupted)
            .await;
    }
    error
}

pub(super) fn stream_credential_failure(
    error: &ProviderError,
    upstream_model: &UpstreamModelId,
) -> Option<GrokCredentialFailure> {
    match error.kind() {
        ProviderErrorKind::Unauthorized => Some(GrokCredentialFailure::Unauthorized),
        ProviderErrorKind::RateLimited => Some(GrokCredentialFailure::RateLimited {
            retry_after: error.retry_after(),
        }),
        ProviderErrorKind::QuotaExhausted => {
            let visible = error.client_visible_upstream_error();
            let quota_kind = classify_grok_quota_failure(
                visible
                    .and_then(ClientVisibleUpstreamError::code)
                    .or_else(|| error.upstream_code().map(|code| code.as_str())),
                visible.and_then(ClientVisibleUpstreamError::error_type),
                visible.map(ClientVisibleUpstreamError::message),
            );
            Some(match quota_kind {
                Some(GrokQuotaFailureKind::FreeAccount) => {
                    GrokCredentialFailure::FreeQuotaExhausted
                }
                Some(GrokQuotaFailureKind::FreeModelUsage) => {
                    GrokCredentialFailure::ModelQuotaExhausted {
                        upstream_model: upstream_model.clone(),
                        retry_after: error.retry_after(),
                    }
                }
                Some(GrokQuotaFailureKind::Account) | None => GrokCredentialFailure::QuotaExhausted,
            })
        }
        _ => None,
    }
}

pub(super) async fn recover_or_record_failure(
    selector: &dyn GrokSessionSelector,
    recovery: &dyn GrokCredentialRecovery,
    session: &SelectedGrokSession,
    error: ProviderError,
    credential_failure: Option<GrokCredentialFailure>,
    recovery_attempted: bool,
) -> ProviderError {
    if !session.allows_account_state_mutation() {
        return error;
    }
    if error.requires_credential_recovery() && !recovery_attempted {
        return match recovery
            .recover_unauthorized(session.account_id(), session.credential_revision())
            .await
        {
            GrokCredentialRecoveryOutcome::Recovered => error.with_same_account_retry(),
            GrokCredentialRecoveryOutcome::Rejected => error,
            GrokCredentialRecoveryOutcome::Unavailable => match credential_failure {
                Some(failure) => record_credential_failure(selector, session, error, failure).await,
                None => error,
            },
        };
    }
    match credential_failure {
        Some(failure) => record_credential_failure(selector, session, error, failure).await,
        None => error,
    }
}

pub(super) fn transport_credential_failure(
    error: &GrokInferenceTransportError,
    upstream_model: &UpstreamModelId,
) -> Option<GrokCredentialFailure> {
    match error.kind() {
        GrokInferenceTransportErrorKind::Unauthorized => Some(GrokCredentialFailure::Unauthorized),
        GrokInferenceTransportErrorKind::PermissionDenied => {
            Some(GrokCredentialFailure::AccessDenied)
        }
        GrokInferenceTransportErrorKind::RateLimited if model_capacity_failure(error) => {
            Some(GrokCredentialFailure::ModelCapacity {
                upstream_model: upstream_model.clone(),
            })
        }
        GrokInferenceTransportErrorKind::RateLimited => Some(GrokCredentialFailure::RateLimited {
            retry_after: error.retry_after(),
        }),
        GrokInferenceTransportErrorKind::QuotaExhausted => {
            Some(GrokCredentialFailure::QuotaExhausted)
        }
        GrokInferenceTransportErrorKind::FreeQuotaExhausted => {
            Some(GrokCredentialFailure::FreeQuotaExhausted)
        }
        GrokInferenceTransportErrorKind::PaymentRequired => {
            Some(GrokCredentialFailure::PaymentRequired {
                retry_after: error.retry_after(),
            })
        }
        GrokInferenceTransportErrorKind::ModelQuotaExhausted => {
            Some(GrokCredentialFailure::ModelQuotaExhausted {
                upstream_model: upstream_model.clone(),
                retry_after: error.retry_after(),
            })
        }
        GrokInferenceTransportErrorKind::ModelAccessDenied => {
            Some(GrokCredentialFailure::ModelAccessDenied {
                upstream_model: upstream_model.clone(),
                retry_after: error.retry_after(),
            })
        }
        GrokInferenceTransportErrorKind::Unavailable if empty_upstream_failure(error) => {
            Some(GrokCredentialFailure::EmptyUpstream)
        }
        GrokInferenceTransportErrorKind::Unavailable => {
            Some(GrokCredentialFailure::UpstreamUnavailable)
        }
        _ => None,
    }
}

pub(super) fn model_capacity_failure(error: &GrokInferenceTransportError) -> bool {
    transport_error_contains_any(
        error,
        &[
            "model_capacity",
            "capacity",
            "overloaded",
            "server_busy",
            "too many concurrent",
            "engine_overloaded",
        ],
    )
}

pub(super) fn empty_upstream_failure(error: &GrokInferenceTransportError) -> bool {
    transport_error_contains_any(
        error,
        &[
            "empty_upstream",
            "empty model output",
            "no content/tool_calls",
            "no client-visible content",
            "empty upstream",
        ],
    )
}

pub(super) fn transport_error_contains_any(
    error: &GrokInferenceTransportError,
    signals: &[&str],
) -> bool {
    let code = error
        .upstream_code()
        .map(|code| code.as_str())
        .unwrap_or_default();
    let message = error
        .client_visible_upstream_error()
        .map(ClientVisibleUpstreamError::message)
        .unwrap_or_default()
        .to_ascii_lowercase();
    signals
        .iter()
        .any(|signal| code.contains(signal) || message.contains(signal))
}

pub(super) async fn map_and_record_stream_transport_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: GrokInferenceTransportError,
    upstream_model: &UpstreamModelId,
) -> ProviderError {
    let request_scoped = error.kind() == GrokInferenceTransportErrorKind::SafetyRejected;
    let credential_failure = transport_credential_failure(&error, upstream_model);
    let error = map_stream_error(error);
    match credential_failure {
        Some(failure) => record_credential_failure(selector, session, error, failure).await,
        None if request_scoped => error,
        None => record_stream_failure(selector, session, error, upstream_model).await,
    }
}

pub(super) fn map_continuation_failure(
    context: &AttemptContext,
    error: ProviderError,
) -> ProviderError {
    let is_reasoning_decode_failure = context.continuation_attempt() == ContinuationAttempt::Native
        && error.kind() == ProviderErrorKind::InvalidRequest
        && error.upstream_status() == Some(400)
        && error
            .upstream_code()
            .is_some_and(|code| code.as_str() == REASONING_DECODE_FAILED_CODE);
    let is_missing_native_response = context.continuation_attempt() == ContinuationAttempt::Native
        && error.kind() == ProviderErrorKind::InvalidRequest
        && error.upstream_status() == Some(404)
        && error
            .upstream_code()
            .is_some_and(|code| code.as_str() == RESPONSE_NOT_FOUND_CODE);
    if is_reasoning_decode_failure || is_missing_native_response {
        error
            .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
            .with_continuation_recovery_disposition(
                ContinuationRecoveryDisposition::ProviderReplayAllowed,
            )
            .with_replay_safe()
    } else {
        error
    }
}

pub(super) fn preflight_context(context: &AttemptContext) -> Result<(), ProviderError> {
    if context.cancellation().is_cancelled() {
        return Err(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::NotSent,
        ));
    }
    if remaining(context.deadline()).is_none() {
        return Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::NotSent,
        ));
    }
    Ok(())
}

pub(super) fn ensure_sent_context(context: &AttemptContext) -> Result<(), ProviderError> {
    if context.cancellation().is_cancelled() {
        return Err(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::Sent,
        ));
    }
    if remaining(context.deadline()).is_none() {
        return Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        ));
    }
    Ok(())
}

pub(super) fn map_request_error(error: GrokRequestEncodeError) -> ProviderError {
    let kind = match error {
        GrokRequestEncodeError::InvalidProtocolPayload
        | GrokRequestEncodeError::InvalidRequestNormalization
        | GrokRequestEncodeError::InvalidRequestField { .. } => ProviderErrorKind::InvalidRequest,
        GrokRequestEncodeError::Serialization => ProviderErrorKind::Protocol,
    };
    let provider_error = provider_error(kind, UpstreamSendState::NotSent);
    if kind != ProviderErrorKind::InvalidRequest {
        return provider_error;
    }
    provider_error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        error.to_string(),
        Some("invalid_request_normalization".to_owned()),
        Some("invalid_request_error".to_owned()),
    ))
}

/// 将选择阶段失败映射为带结构化 code 与 retry_after 的 Provider 错误。
pub(super) fn map_selection_error(error: GrokSessionSelectorError) -> ProviderError {
    let (retry_after, message, code) = match error {
        GrokSessionSelectorError::AccountCoolingDown { retry_after } => (
            retry_after,
            cooling_down_message(retry_after),
            "account_cooling_down",
        ),
        GrokSessionSelectorError::ModelCoolingDown { retry_after } => (
            retry_after,
            model_cooling_down_message(retry_after),
            "model_cooling_down",
        ),
        GrokSessionSelectorError::CapacityUnavailable { retry_after } => (
            retry_after,
            "account is at its concurrency or request-interval limit".to_owned(),
            "account_capacity_busy",
        ),
        GrokSessionSelectorError::NoEligibleSession => (
            None,
            "no account is eligible for the requested model".to_owned(),
            "no_eligible_account",
        ),
        GrokSessionSelectorError::Unavailable => (
            None,
            "account scheduling state is temporarily unreadable".to_owned(),
            "account_selector_unavailable",
        ),
        GrokSessionSelectorError::InvalidSession => {
            return provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent);
        }
    };
    let error = provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent);
    let error = match retry_after {
        Some(retry_after) => error.with_retry_after(retry_after),
        None => error,
    };
    error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        message,
        Some(code.to_owned()),
        Some("account_unavailable_error".to_owned()),
    ))
}

pub(super) fn cooling_down_message(retry_after: Option<Duration>) -> String {
    match retry_after {
        Some(retry_after) => format!(
            "account is cooling down after an upstream failure; retry in {}s",
            retry_after.as_secs().saturating_add(1)
        ),
        None => "account is cooling down after an upstream failure".to_owned(),
    }
}

pub(super) fn model_cooling_down_message(retry_after: Option<Duration>) -> String {
    match retry_after {
        Some(retry_after) => format!(
            "all eligible accounts are cooling down for this model; retry in {}s",
            retry_after.as_secs().saturating_add(1)
        ),
        None => "all eligible accounts are cooling down for this model".to_owned(),
    }
}

pub(super) fn map_transport_error_for_context(
    error: GrokInferenceTransportError,
    context: &AttemptContext,
) -> ProviderError {
    let allow_explicit_replay = context.continuation().is_none()
        || error.kind() == GrokInferenceTransportErrorKind::Unauthorized;
    map_transport_error_with_state(error, None, allow_explicit_replay)
}

pub(super) fn map_stream_error(error: GrokInferenceTransportError) -> ProviderError {
    map_transport_error_with_state(error, Some(UpstreamSendState::Sent), false)
}

pub(super) fn map_transport_error_with_state(
    error: GrokInferenceTransportError,
    forced_send_state: Option<UpstreamSendState>,
    allow_explicit_replay: bool,
) -> ProviderError {
    let transport_kind = error.kind();
    let kind = match transport_kind {
        GrokInferenceTransportErrorKind::InvalidRequest => ProviderErrorKind::InvalidRequest,
        GrokInferenceTransportErrorKind::Unsupported => ProviderErrorKind::Unsupported,
        GrokInferenceTransportErrorKind::Unauthorized => ProviderErrorKind::Unauthorized,
        GrokInferenceTransportErrorKind::PermissionDenied
        | GrokInferenceTransportErrorKind::ModelAccessDenied
        | GrokInferenceTransportErrorKind::PaymentRequired
        | GrokInferenceTransportErrorKind::SafetyRejected => ProviderErrorKind::PermissionDenied,
        GrokInferenceTransportErrorKind::RateLimited => ProviderErrorKind::RateLimited,
        GrokInferenceTransportErrorKind::QuotaExhausted
        | GrokInferenceTransportErrorKind::FreeQuotaExhausted
        | GrokInferenceTransportErrorKind::ModelQuotaExhausted => ProviderErrorKind::QuotaExhausted,
        GrokInferenceTransportErrorKind::Timeout => ProviderErrorKind::Timeout,
        GrokInferenceTransportErrorKind::Transport => ProviderErrorKind::Transport,
        GrokInferenceTransportErrorKind::Protocol => ProviderErrorKind::Protocol,
        GrokInferenceTransportErrorKind::Unavailable => ProviderErrorKind::Unavailable,
        GrokInferenceTransportErrorKind::Cancelled => ProviderErrorKind::Cancelled,
    };
    let mut mapped = provider_error(
        kind,
        forced_send_state.unwrap_or_else(|| error.send_state()),
    );
    if let Some(status) = error.status() {
        mapped = mapped.with_status(status);
        if allow_explicit_replay
            && forced_send_state.is_none()
            && explicit_rejection_is_replay_safe(&error, status)
        {
            mapped = mapped.with_replay_safe();
        }
    }
    if let Some(retry_after) = error.retry_after() {
        mapped = mapped.with_retry_after(retry_after);
    }
    if let Some(request_id) = error.request_id().cloned() {
        mapped = mapped.with_upstream_request_id(request_id);
    }
    if let Some(code) = error.upstream_code().cloned() {
        mapped = mapped.with_upstream_code(code);
    }
    if let Some(detail) = error.client_visible_upstream_error().cloned() {
        mapped = mapped.with_client_visible_upstream_error(detail);
    }
    if error.requires_credential_recovery() {
        mapped = mapped.with_credential_recovery().with_replay_safe();
    }
    if error.sensitive_context_was_redacted() {
        mapped = mapped.redact_sensitive_context("upstream transport context");
    }
    mapped
}

pub(super) fn explicit_rejection_is_replay_safe(
    error: &GrokInferenceTransportError,
    status: u16,
) -> bool {
    let kind = error.kind();
    if kind == GrokInferenceTransportErrorKind::SafetyRejected {
        return false;
    }
    if matches!(status, 401 | 402 | 403 | 405 | 429 | 529) || (500..=599).contains(&status) {
        return true;
    }
    status == 400
        && (matches!(
            kind,
            GrokInferenceTransportErrorKind::QuotaExhausted
                | GrokInferenceTransportErrorKind::FreeQuotaExhausted
                | GrokInferenceTransportErrorKind::ModelQuotaExhausted
                | GrokInferenceTransportErrorKind::PaymentRequired
                | GrokInferenceTransportErrorKind::Unavailable
        ) || (kind == GrokInferenceTransportErrorKind::RateLimited
            && model_capacity_failure(error)))
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
