//! 账号级失败分类、隔离副作用与换号决策 owner。

use crate::{
    dispatch::{
        controllers::ControllerFailureFact,
        errors::ClientFailure,
        failure::exhaustion::{AccountExhaustionRecord, ExhaustedAccountKind},
        lifecycle::contract::{
            AttemptDecision, AttemptObservation, AttemptObservationKind, CompleteResponseFacts,
        },
        transport::observation::{UpstreamFailureFacts, UpstreamFailureKind},
    },
    fleet::{account::AccountStatus, pool::AccountPoolService},
    upstream::openai::protocol::responses::ResponsesSseFailure,
};

pub(super) struct AccountFailureController;

pub(super) struct ClassifiedFailure {
    exhaustion: AccountExhaustionRecord,
    effect: AccountEffect,
}

enum AccountEffect {
    None,
    SetStatus(AccountStatus),
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
        classified: &ClassifiedFailure,
    ) {
        let Some(account_id) = classified.exhaustion.account_id.as_deref() else {
            return;
        };
        match &classified.effect {
            AccountEffect::None => {}
            AccountEffect::SetStatus(status) => {
                account_pool.set_status(account_id, *status).await;
            }
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
        if is_auth_failure(&code, &message) {
            return Some(ClientFailure::new(failure.clone(), 401, false));
        }
        if code.contains("forbidden") || code.contains("banned") {
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
                effect: AccountEffect::None,
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
    let lower = body.to_ascii_lowercase();
    let (kind, effect, message, status_code) = if (status == Some(403) && !is_html(body))
        || (status == Some(402) && is_deactivated_workspace(body))
    {
        (
            ExhaustedAccountKind::Banned,
            AccountEffect::SetStatus(AccountStatus::Banned),
            body.to_string(),
            status,
        )
    } else if status == Some(401) {
        let account_status = if lower.contains("banned") || lower.contains("deactivated") {
            AccountStatus::Banned
        } else {
            AccountStatus::Expired
        };
        (
            if account_status == AccountStatus::Banned {
                ExhaustedAccountKind::Banned
            } else {
                ExhaustedAccountKind::Expired
            },
            AccountEffect::SetStatus(account_status),
            body.to_string(),
            status,
        )
    } else if status.is_some_and(|status| {
        (400..=499).contains(&status) && !matches!(status, 401 | 402 | 403 | 404 | 429)
    }) && is_model_unsupported(body)
    {
        (
            ExhaustedAccountKind::ModelUnsupported,
            AccountEffect::None,
            body.to_string(),
            None,
        )
    } else {
        return None;
    };
    let mut exhaustion = AccountExhaustionRecord::new(account_id, kind, message);
    if let Some(status_code) = status_code {
        exhaustion = exhaustion.with_status_code(status_code);
    }
    Some(ClassifiedFailure { exhaustion, effect })
}

fn classify_sse(account_id: &str, failure: &ResponsesSseFailure) -> Option<ClassifiedFailure> {
    let code = failure.upstream_code.as_deref().unwrap_or_default();
    let lower_message = failure.message.to_ascii_lowercase();
    let (kind, effect, status) = if is_model_unsupported(code)
        || is_model_unsupported(&failure.message)
    {
        (
            ExhaustedAccountKind::ModelUnsupported,
            AccountEffect::None,
            None,
        )
    } else if is_auth_failure(code, &lower_message) {
        let account_status = if lower_message.contains("banned") || code == "account_deactivated" {
            AccountStatus::Banned
        } else {
            AccountStatus::Expired
        };
        (
            if account_status == AccountStatus::Banned {
                ExhaustedAccountKind::Banned
            } else {
                ExhaustedAccountKind::Expired
            },
            AccountEffect::SetStatus(account_status),
            Some(if account_status == AccountStatus::Banned {
                403
            } else {
                401
            }),
        )
    } else {
        return None;
    };
    let mut exhaustion = AccountExhaustionRecord::new(
        account_id,
        kind,
        crate::dispatch::failure::sse::sse_failure_error_body(failure),
    );
    if let Some(status) = status {
        exhaustion = exhaustion.with_status_code(status);
    }
    Some(ClassifiedFailure { exhaustion, effect })
}

fn is_auth_failure(code: &str, message: &str) -> bool {
    matches!(
        code,
        "token_invalid"
            | "token_expired"
            | "token_revoked"
            | "account_deactivated"
            | "unauthorized"
            | "invalid_api_key"
    ) || message.contains("token revoked")
        || message.contains("token invalid")
        || message.contains("token expired")
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

fn is_deactivated_workspace(value: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(value).is_ok_and(|value| {
        value
            .pointer("/detail/code")
            .and_then(serde_json::Value::as_str)
            == Some("deactivated_workspace")
    })
}
