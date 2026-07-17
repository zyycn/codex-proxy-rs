//! 账号级失败分类、隔离副作用与换号决策 owner。

use crate::{
    dispatch::{
        controllers::ControllerFailureFact,
        errors::ClientFailure,
        failure::exhaustion::{AccountExhaustionRecord, ExhaustedAccountKind},
        lifecycle::contract::{
            AttemptDecision, AttemptObservation, AttemptObservationKind, CompleteResponseFacts,
        },
    },
    fleet::{
        account_failure::{
            AccountFailureKind, AccountStateEffect, apply_account_state_effect_immediately,
            classify_account_failure,
        },
        pool::AccountPoolService,
    },
    upstream::openai::{
        failure::{UpstreamFailureFacts, UpstreamFailureKind},
        protocol::responses::ResponsesSseFailure,
        transport::CodexBackendClient,
    },
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
            apply_account_state_effect_immediately(account_pool, codex, account_id, effect).await;
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
        if code.contains("invalid_request")
            || code.contains("not_found")
            || code.contains("context_window")
            || code == "context_length_exceeded"
            || code.contains("invalid_prompt")
            || code.contains("bad_request")
        {
            return Some(ClientFailure::new(failure.clone(), 400, false));
        }
        if matches!(code.as_str(), "server_is_overloaded" | "slow_down")
            || code.contains("server_overloaded")
        {
            return Some(ClientFailure::new(failure.clone(), 503, true));
        }
        if code == "usage_not_included" {
            return Some(ClientFailure::new(failure.clone(), 403, false));
        }
        classify_account_failure(&super::failure_observation::from_response(failure)).map(
            |classified| {
                let status = match classified.kind {
                    AccountFailureKind::ModelUnsupported => 400,
                    AccountFailureKind::Expired => 401,
                    AccountFailureKind::Disabled | AccountFailureKind::Banned => 403,
                    AccountFailureKind::QuotaExhausted | AccountFailureKind::RateLimited => 429,
                };
                ClientFailure::new(failure.clone(), status, false)
            },
        )
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
    classified_failure(
        account_id,
        &facts.body,
        facts.status_code,
        classify_account_failure(&super::failure_observation::from_upstream(facts))?,
    )
}

fn classify_sse(account_id: &str, failure: &ResponsesSseFailure) -> Option<ClassifiedFailure> {
    classified_failure(
        account_id,
        &crate::dispatch::failure::sse::sse_failure_error_body(failure),
        failure.explicit_status_code,
        classify_account_failure(&super::failure_observation::from_response(failure))?,
    )
}

fn classified_failure(
    account_id: &str,
    message: &str,
    status_code: Option<u16>,
    classified: crate::fleet::account_failure::ClassifiedAccountFailure,
) -> Option<ClassifiedFailure> {
    let kind = match classified.kind {
        AccountFailureKind::ModelUnsupported => ExhaustedAccountKind::ModelUnsupported,
        AccountFailureKind::Expired => ExhaustedAccountKind::Expired,
        AccountFailureKind::Disabled => ExhaustedAccountKind::Disabled,
        AccountFailureKind::Banned => ExhaustedAccountKind::Banned,
        AccountFailureKind::QuotaExhausted | AccountFailureKind::RateLimited => return None,
    };
    let mut exhaustion = AccountExhaustionRecord::new(account_id, kind, message);
    if let Some(status_code) = status_code {
        exhaustion = exhaustion.with_status_code(status_code);
    }
    Some(ClassifiedFailure {
        exhaustion,
        effect: classified.effect,
    })
}
