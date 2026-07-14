//! 账号级失败分类、隔离副作用与换号决策 owner。

use crate::{
    dispatch::{
        controllers::{
            ControllerFailureFact,
            account_state::{AccountStateEffect, AccountStateEffects},
        },
        errors::ClientFailure,
        failure::exhaustion::{AccountExhaustionRecord, ExhaustedAccountKind},
        lifecycle::contract::{
            AttemptDecision, AttemptObservation, AttemptObservationKind, CompleteResponseFacts,
        },
        transport::observation::{UpstreamFailureFacts, UpstreamFailureKind},
    },
    fleet::{account::AccountStatus, pool::AccountPoolService},
    upstream::openai::{protocol::responses::ResponsesSseFailure, transport::CodexBackendClient},
};

pub(super) struct AccountFailureController;

pub(super) struct ClassifiedFailure {
    exhaustion: AccountExhaustionRecord,
    effect: Option<AccountStateEffect>,
}

impl AccountFailureController {
    pub(super) fn classify(observation: &AttemptObservation) -> Option<ClassifiedFailure> {
        let account_id = observation.account.as_ref()?.id.as_str();
        classify(observation, account_id)
    }

    pub(super) fn classify_failure(
        account_id: &str,
        failure: ControllerFailureFact<'_>,
    ) -> Option<ClassifiedFailure> {
        classify_failure(account_id, failure)
    }

    pub(super) async fn apply_effect(
        account_pool: &AccountPoolService,
        codex: &CodexBackendClient,
        classified: &ClassifiedFailure,
    ) {
        let Some(account_id) = classified.exhaustion.account_id.as_deref() else {
            return;
        };
        if let Some(effect) = &classified.effect {
            AccountStateEffects::apply(account_pool, codex, account_id, effect).await;
        }
    }

    pub(super) fn decision(
        observation: &AttemptObservation,
        classified: ClassifiedFailure,
    ) -> AttemptDecision {
        if observation.routing.can_retry_next_candidate {
            AttemptDecision::RetryNextCandidate {
                exhaustion: Some(classified.exhaustion),
                on_exhaustion: None,
            }
        } else {
            AttemptDecision::Return(
                crate::dispatch::lifecycle::contract::AttemptReturnKind::Observed,
            )
        }
    }

    pub(super) fn is_retryable_transport(observation: &AttemptObservation) -> bool {
        let AttemptObservationKind::UpstreamFailure(facts) = &observation.kind else {
            return false;
        };
        matches!(
            facts.kind,
            UpstreamFailureKind::HttpConnect
                | UpstreamFailureKind::HttpTimeout
                | UpstreamFailureKind::HttpTransport
                | UpstreamFailureKind::StreamIdle
                | UpstreamFailureKind::WebSocketTransport
                | UpstreamFailureKind::WebSocketTimeout
        ) || facts
            .status_code
            .is_some_and(|status| (500..=599).contains(&status))
    }

    pub(super) fn client_failure(
        classified: &ClassifiedFailure,
        failure: &ResponsesSseFailure,
    ) -> ClientFailure {
        let status_code =
            classified
                .exhaustion
                .status_code
                .unwrap_or(match classified.exhaustion.kind {
                    ExhaustedAccountKind::ModelUnsupported => 400,
                    ExhaustedAccountKind::Expired | ExhaustedAccountKind::Disabled => 401,
                    ExhaustedAccountKind::Banned => 403,
                    ExhaustedAccountKind::QuotaExhausted
                    | ExhaustedAccountKind::RateLimited
                    | ExhaustedAccountKind::CloudflareChallenge
                    | ExhaustedAccountKind::CloudflarePathBlocked
                    | ExhaustedAccountKind::UpstreamUnavailable => 502,
                });
        ClientFailure::new(failure.clone(), status_code, false)
    }

    pub(super) fn unowned_client_failure(failure: &ResponsesSseFailure) -> Option<ClientFailure> {
        let code = failure
            .upstream_code
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let error_type = failure
            .upstream_type
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let message = failure.message.to_ascii_lowercase();
        if is_model_unsupported(&code)
            || is_model_unsupported(&message)
            || code.contains("invalid_request")
            || code.contains("not_found")
            || code.contains("context_window")
            || code.contains("invalid_prompt")
            || code.contains("bad_request")
        {
            return Some(ClientFailure::new(failure.clone(), 400, false));
        }
        if is_auth_failure(&code, &error_type, &message) {
            return Some(ClientFailure::new(failure.clone(), 401, false));
        }
        if code.contains("forbidden") || code.contains("banned") || error_type == "permission_error"
        {
            return Some(ClientFailure::new(failure.clone(), 403, false));
        }
        if code.contains("server_overloaded") {
            return Some(ClientFailure::new(failure.clone(), 503, true));
        }
        None
    }
}

fn classify(observation: &AttemptObservation, account_id: &str) -> Option<ClassifiedFailure> {
    match &observation.kind {
        AttemptObservationKind::CompleteResponse(CompleteResponseFacts::Empty) => {
            Some(ClassifiedFailure {
                exhaustion: AccountExhaustionRecord::new(
                    account_id,
                    ExhaustedAccountKind::UpstreamUnavailable,
                    "upstream response did not include visible output",
                ),
                effect: None,
            })
        }
        _ => ControllerFailureFact::from_attempt(observation)
            .and_then(|failure| classify_failure(account_id, failure)),
    }
}

fn classify_failure(
    account_id: &str,
    failure: ControllerFailureFact<'_>,
) -> Option<ClassifiedFailure> {
    match failure {
        ControllerFailureFact::Upstream(facts) => classify_upstream(account_id, facts),
        ControllerFailureFact::Response(failure) => classify_sse(account_id, failure),
    }
}

fn classify_upstream(account_id: &str, facts: &UpstreamFailureFacts) -> Option<ClassifiedFailure> {
    let status = facts.status_code;
    let body = facts.body.as_str();
    let code = facts.code.as_deref().unwrap_or_default();
    let error_type = facts.error_type.as_deref().unwrap_or_default();
    let message = facts.message.as_str();

    if is_model_unsupported(code) || is_model_unsupported(message) || is_model_unsupported(body) {
        return Some(model_unsupported_failure(account_id, body));
    }
    if let Some(account_status) = explicit_account_status(code, error_type, body) {
        return Some(account_status_failure(
            account_id,
            account_status,
            body,
            status,
        ));
    }
    if status == Some(403) && !is_html(body) {
        return Some(account_status_failure(
            account_id,
            AccountStatus::Banned,
            body,
            status,
        ));
    }
    if status == Some(401) {
        return Some(account_status_failure(
            account_id,
            AccountStatus::Expired,
            body,
            status,
        ));
    }
    None
}

fn classify_sse(account_id: &str, failure: &ResponsesSseFailure) -> Option<ClassifiedFailure> {
    let code = failure.upstream_code.as_deref().unwrap_or_default();
    let error_type = failure.upstream_type.as_deref().unwrap_or_default();
    if is_model_unsupported(code) || is_model_unsupported(&failure.message) {
        return Some(model_unsupported_failure(
            account_id,
            &crate::dispatch::failure::sse::sse_failure_error_body(failure),
        ));
    }
    let account_status =
        explicit_account_status(code, error_type, &failure.message).or_else(|| {
            (code.eq_ignore_ascii_case("forbidden") || error_type == "permission_error")
                .then_some(AccountStatus::Banned)
        })?;
    Some(account_status_failure(
        account_id,
        account_status,
        &crate::dispatch::failure::sse::sse_failure_error_body(failure),
        failure.explicit_status_code,
    ))
}

fn model_unsupported_failure(account_id: &str, message: &str) -> ClassifiedFailure {
    ClassifiedFailure {
        exhaustion: AccountExhaustionRecord::new(
            account_id,
            ExhaustedAccountKind::ModelUnsupported,
            message,
        ),
        effect: None,
    }
}

fn account_status_failure(
    account_id: &str,
    status: AccountStatus,
    message: &str,
    status_code: Option<u16>,
) -> ClassifiedFailure {
    let kind = match status {
        AccountStatus::Expired => ExhaustedAccountKind::Expired,
        AccountStatus::Disabled => ExhaustedAccountKind::Disabled,
        AccountStatus::Banned => ExhaustedAccountKind::Banned,
        AccountStatus::Active | AccountStatus::QuotaExhausted => {
            unreachable!("account failure controller only invalidates unavailable statuses")
        }
    };
    let mut exhaustion = AccountExhaustionRecord::new(account_id, kind, message);
    if let Some(status_code) = status_code {
        exhaustion = exhaustion.with_status_code(status_code);
    }
    ClassifiedFailure {
        exhaustion,
        effect: Some(AccountStateEffect::SetStatus(status)),
    }
}

fn explicit_account_status(code: &str, error_type: &str, message: &str) -> Option<AccountStatus> {
    let code = code.trim().to_ascii_lowercase();
    let error_type = error_type.trim().to_ascii_lowercase();
    let message = message.to_ascii_lowercase();
    if matches!(
        code.as_str(),
        "identity_verification_required" | "verification_required"
    ) || message.contains("identity verification is required")
    {
        return Some(AccountStatus::Disabled);
    }
    if matches!(
        code.as_str(),
        "account_banned"
            | "account_deactivated"
            | "account_disabled"
            | "account_suspended"
            | "deactivated_workspace"
            | "organization_disabled"
            | "workspace_deactivated"
    ) || message.contains("account is banned")
        || message.contains("account has been banned")
        || message.contains("account deactivated")
        || message.contains("account has been deactivated")
        || message.contains("account disabled")
        || message.contains("account has been disabled")
        || message.contains("account suspended")
        || message.contains("organization has been disabled")
        || message.contains("workspace has been deactivated")
        || message.contains("deactivated_workspace")
    {
        return Some(AccountStatus::Banned);
    }
    is_auth_failure(&code, &error_type, &message).then_some(AccountStatus::Expired)
}

fn is_auth_failure(code: &str, error_type: &str, message: &str) -> bool {
    matches!(
        code,
        "token_invalid"
            | "token_invalidated"
            | "token_expired"
            | "token_revoked"
            | "refresh_token_invalidated"
            | "unauthorized"
            | "invalid_api_key"
            | "authentication_error"
    ) || error_type == "authentication_error"
        || message.contains("token revoked")
        || message.contains("token invalidated")
        || message.contains("token invalid")
        || message.contains("token expired")
        || message.contains("unauthorized")
        || message.contains("invalid api key")
}

fn is_model_unsupported(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("model_not_supported")
        || value.contains("model_not_available")
        || (value.contains("model")
            && (value.contains("not supported")
                || value.contains("not available")
                || value.contains("not_supported")
                || value.contains("not_available")))
}

fn is_html(value: &str) -> bool {
    let value = value.trim_start().to_ascii_lowercase();
    value.starts_with("<!doctype") || value.starts_with("<html") || value.contains("<html")
}
