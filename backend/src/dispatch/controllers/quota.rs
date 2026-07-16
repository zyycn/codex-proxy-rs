//! 配额失败规则 owner。

use crate::{
    dispatch::{
        controllers::ControllerFailureFact,
        errors::ClientFailure,
        failure::exhaustion::{AccountExhaustionRecord, ExhaustedAccountKind},
        lifecycle::contract::{AttemptDecision, AttemptObservation, AttemptReturnKind},
    },
    fleet::{
        account_failure::{
            AccountFailureKind, AccountStateEffect, apply_account_state_effect_immediately,
            classify_response_failure, classify_upstream_failure,
        },
        pool::AccountPoolService,
    },
    upstream::openai::{
        failure::UpstreamFailureFacts, protocol::responses::ResponsesSseFailure,
        transport::CodexBackendClient,
    },
};

pub(super) struct QuotaController;

impl QuotaController {
    pub(super) fn classify(observation: &AttemptObservation) -> Option<ClassifiedQuotaFailure> {
        classify(observation)
    }

    pub(super) fn classify_failure(
        account_id: &str,
        failure: ControllerFailureFact<'_>,
    ) -> Option<ClassifiedQuotaFailure> {
        classify_failure(account_id, failure)
    }

    pub(super) fn decision(
        observation: &AttemptObservation,
        classified: ClassifiedQuotaFailure,
    ) -> AttemptDecision {
        if observation.routing.can_retry_next_candidate {
            return AttemptDecision::RetryNextCandidate {
                exhaustion: Some(classified.exhaustion),
                on_exhaustion: None,
            };
        }
        AttemptDecision::Return(AttemptReturnKind::Observed)
    }

    pub(super) async fn apply_effect(
        account_pool: &AccountPoolService,
        codex: &CodexBackendClient,
        classified: &ClassifiedQuotaFailure,
    ) {
        let Some(account_id) = classified.exhaustion.account_id.as_deref() else {
            return;
        };
        if let Some(effect) = &classified.effect {
            apply_account_state_effect_immediately(account_pool, codex, account_id, effect).await;
        }
    }

    pub(super) fn client_failure(failure: &ResponsesSseFailure) -> ClientFailure {
        ClientFailure::new(failure.clone(), 429, false)
    }
}

pub(super) struct ClassifiedQuotaFailure {
    exhaustion: AccountExhaustionRecord,
    effect: Option<AccountStateEffect>,
}

fn classify(observation: &AttemptObservation) -> Option<ClassifiedQuotaFailure> {
    let account_id = observation.account.as_ref()?.id.as_str();
    ControllerFailureFact::from_attempt(observation)
        .and_then(|failure| classify_failure(account_id, failure))
}

fn classify_failure(
    account_id: &str,
    failure: ControllerFailureFact<'_>,
) -> Option<ClassifiedQuotaFailure> {
    match failure {
        ControllerFailureFact::Upstream(facts) => classify_upstream(account_id, facts),
        ControllerFailureFact::Response(failure) => classify_stream(account_id, failure),
    }
}

fn classify_upstream(
    account_id: &str,
    facts: &UpstreamFailureFacts,
) -> Option<ClassifiedQuotaFailure> {
    classified_quota_failure(account_id, &facts.body, classify_upstream_failure(facts)?)
}

fn classify_stream(
    account_id: &str,
    failure: &ResponsesSseFailure,
) -> Option<ClassifiedQuotaFailure> {
    let body = crate::dispatch::failure::sse::sse_failure_error_body(failure);
    classified_quota_failure(account_id, &body, classify_response_failure(failure)?)
}

fn classified_quota_failure(
    account_id: &str,
    message: &str,
    classified: crate::fleet::account_failure::ClassifiedAccountFailure,
) -> Option<ClassifiedQuotaFailure> {
    let kind = match classified.kind {
        AccountFailureKind::RateLimited => ExhaustedAccountKind::RateLimited,
        AccountFailureKind::QuotaExhausted => ExhaustedAccountKind::QuotaExhausted,
        AccountFailureKind::ModelUnsupported
        | AccountFailureKind::Expired
        | AccountFailureKind::Disabled
        | AccountFailureKind::Banned => return None,
    };
    Some(ClassifiedQuotaFailure {
        exhaustion: AccountExhaustionRecord::new(account_id, kind, message),
        effect: classified.effect,
    })
}
