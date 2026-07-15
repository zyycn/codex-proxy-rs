//! 请求前配额验证规则 owner。

use serde_json::Value;

use crate::{
    dispatch::{
        affinity::AccountIdentityService,
        controllers::ControllerFailureFact,
        errors::ClientFailure,
        failure::exhaustion::{AccountExhaustionRecord, ExhaustedAccountKind},
        lifecycle::contract::{
            AttemptDecision, AttemptObservation, AttemptObservationKind, AttemptReturnKind,
        },
    },
    fleet::{
        account_failure::{
            AccountFailureKind, AccountStateEffect, apply_account_state_effect_immediately,
            classify_response_failure, classify_upstream_failure,
        },
        pool::{AccountLease, AccountPoolService},
        quota::{
            quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
            quota_snapshot_reset_at,
        },
    },
    upstream::openai::{
        failure::UpstreamFailureFacts,
        protocol::responses::ResponsesSseFailure,
        transport::{CodexBackendClient, CodexRequestContext},
    },
};

const LIMIT_REACHED_MESSAGE: &str = "Upstream usage quota still reports limit_reached";

pub(super) enum QuotaEnterOutcome {
    Ready(Box<AccountLease>),
    LimitReached,
}

pub(super) struct QuotaEnterContext<'a> {
    pub account_pool: &'a AccountPoolService,
    pub codex: &'a CodexBackendClient,
    pub account_identity: &'a AccountIdentityService,
    pub request_id: &'a str,
    pub cookie_header: Option<&'a str>,
}

pub(super) struct QuotaController;

impl QuotaController {
    pub(super) async fn enter(
        context: QuotaEnterContext<'_>,
        acquired: AccountLease,
    ) -> QuotaEnterOutcome {
        if !acquired.account.quota_verify_required {
            return QuotaEnterOutcome::Ready(Box::new(acquired));
        }

        let account_id = acquired.account.id.clone();
        let identity = context.account_identity.scope_auxiliary(&account_id);
        let usage = context
            .codex
            .fetch_usage(CodexRequestContext {
                access_token: &acquired.account.access_token,
                account_id: acquired.account.account_id.as_deref(),
                request_id: context.request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: context.cookie_header,
                installation_id: Some(&identity.installation_id),
                session_id: None,
                thread_id: None,
                client_request_id: None,
                turn_id: None,
            })
            .await;

        let raw = match usage {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(
                    request_id = %context.request_id,
                    account_id = %account_id,
                    quota_verify_required = true,
                    quota_verify_result = "upstream_error",
                    retry_with_another_account = false,
                    error = %error,
                    "Failed to verify stale quota state before upstream request"
                );
                return QuotaEnterOutcome::Ready(Box::new(acquired));
            }
        };

        let quota = quota_from_usage(&raw);
        context
            .account_pool
            .apply_quota_snapshot(&account_id, &quota)
            .await;
        if quota_snapshot_limit_reached(&quota) {
            acquired.release_without_usage().await;
            tracing::info!(
                request_id = %context.request_id,
                account_id = %account_id,
                quota_verify_required = true,
                quota_verify_result = "limit_reached",
                retry_with_another_account = true,
                "Quota verification reported exhausted account before upstream request"
            );
            return QuotaEnterOutcome::LimitReached;
        }

        QuotaEnterOutcome::Ready(Box::new(acquired_with_verified_quota(acquired, &quota)))
    }

    pub(super) fn classify(observation: &AttemptObservation) -> Option<ClassifiedQuotaFailure> {
        if matches!(
            observation.kind,
            AttemptObservationKind::CandidatePreparationRejected
        ) {
            return observation
                .account
                .as_ref()
                .map(|account| ClassifiedQuotaFailure {
                    exhaustion: AccountExhaustionRecord::new(
                        account.id.clone(),
                        ExhaustedAccountKind::RateLimited,
                        LIMIT_REACHED_MESSAGE,
                    ),
                    effect: None,
                });
        }
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

fn acquired_with_verified_quota(mut acquired: AccountLease, quota: &Value) -> AccountLease {
    let limit_reached = quota_snapshot_limit_reached(quota);
    acquired.account.quota_verify_required = false;
    acquired.account.quota_limit_reached = limit_reached;
    acquired.account.quota_cooldown_until = limit_reached
        .then_some(quota_snapshot_reset_at(quota))
        .flatten();
    if let Some(reset_at) = quota_snapshot_reset_at(quota) {
        acquired.account.window_reset_at = Some(reset_at);
        if let Some(limit_window_seconds) = quota_snapshot_limit_window_seconds(quota) {
            acquired.account.limit_window_seconds = Some(limit_window_seconds);
        }
    }
    acquired
}
