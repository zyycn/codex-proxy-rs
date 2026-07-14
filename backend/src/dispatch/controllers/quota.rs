//! 请求前配额验证规则 owner。

use chrono::{Duration, Utc};
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
        transport::observation::UpstreamFailureFacts,
    },
    fleet::{
        pool::{AccountLease, AccountPoolService},
        quota::{
            quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
            quota_snapshot_reset_at,
        },
    },
    upstream::openai::{
        protocol::responses::ResponsesSseFailure,
        transport::{CodexBackendClient, CodexRequestContext},
    },
};

const LIMIT_REACHED_MESSAGE: &str = "Upstream usage quota still reports limit_reached";
const DEFAULT_RATE_LIMIT_RETRY_SECONDS: u64 = 60;

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
        let identity = context
            .account_identity
            .scope_auxiliary(&account_id, context.request_id);
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
                prompt_cache_key: None,
                client_request_id: Some(&identity.client_request_id),
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
                    effect: QuotaEffect::None,
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
        classified: &ClassifiedQuotaFailure,
    ) {
        let Some(account_id) = classified.exhaustion.account_id.as_deref() else {
            return;
        };
        match classified.effect {
            QuotaEffect::None => {}
            QuotaEffect::SetExhausted => {
                account_pool
                    .set_status(
                        account_id,
                        crate::fleet::account::AccountStatus::QuotaExhausted,
                    )
                    .await;
            }
            QuotaEffect::MarkLimitedUntil(until) => {
                account_pool
                    .mark_quota_limited_until(account_id, until)
                    .await;
            }
        }
    }

    pub(super) fn client_failure(failure: &ResponsesSseFailure) -> ClientFailure {
        ClientFailure::new(failure.clone(), 429, false)
    }
}

enum QuotaEffect {
    None,
    SetExhausted,
    MarkLimitedUntil(chrono::DateTime<Utc>),
}

pub(super) struct ClassifiedQuotaFailure {
    exhaustion: AccountExhaustionRecord,
    effect: QuotaEffect,
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
    if facts.status_code == Some(429) {
        return Some(rate_limited_failure(
            account_id,
            facts.body.clone(),
            facts.retry_after_seconds,
        ));
    }
    if facts.status_code == Some(402) {
        return Some(quota_exhausted_failure(account_id, facts.body.clone()));
    }
    None
}

fn classify_stream(
    account_id: &str,
    failure: &ResponsesSseFailure,
) -> Option<ClassifiedQuotaFailure> {
    let kind = match failure.explicit_status_code {
        Some(429) => StreamQuotaKind::RateLimited,
        Some(402) => StreamQuotaKind::QuotaExhausted,
        Some(_) => return None,
        None => classify_stream_signal(failure)?,
    };
    let body = crate::dispatch::failure::sse::sse_failure_error_body(failure);
    Some(match kind {
        StreamQuotaKind::RateLimited => {
            rate_limited_failure(account_id, body, failure.retry_after_seconds)
        }
        StreamQuotaKind::QuotaExhausted => quota_exhausted_failure(account_id, body),
    })
}

#[derive(Clone, Copy)]
enum StreamQuotaKind {
    RateLimited,
    QuotaExhausted,
}

fn classify_stream_signal(failure: &ResponsesSseFailure) -> Option<StreamQuotaKind> {
    failure
        .upstream_code
        .as_deref()
        .and_then(classify_quota_signal)
        .or_else(|| {
            failure
                .upstream_type
                .as_deref()
                .and_then(classify_quota_signal)
        })
        .or_else(|| classify_quota_message(&failure.message))
}

fn classify_quota_signal(signal: &str) -> Option<StreamQuotaKind> {
    let signal = signal.trim().to_ascii_lowercase();
    match signal.as_str() {
        "usage_limit_reached"
        | "rate_limit_exceeded"
        | "rate_limit_reached"
        | "rate_limit_error" => Some(StreamQuotaKind::RateLimited),
        "quota_exhausted" | "quota_exceeded" | "payment_required" | "insufficient_quota" => {
            Some(StreamQuotaKind::QuotaExhausted)
        }
        signal if signal.starts_with("billing_limit") => Some(StreamQuotaKind::QuotaExhausted),
        _ => None,
    }
}

fn classify_quota_message(message: &str) -> Option<StreamQuotaKind> {
    let message = message.to_ascii_lowercase();
    if message.contains("rate limit") || message.contains("usage limit") {
        return Some(StreamQuotaKind::RateLimited);
    }
    if message.contains("quota")
        || message.contains("payment required")
        || message.contains("billing limit")
    {
        return Some(StreamQuotaKind::QuotaExhausted);
    }
    None
}

fn rate_limited_failure(
    account_id: &str,
    message: String,
    retry_after_seconds: Option<u64>,
) -> ClassifiedQuotaFailure {
    let seconds = retry_after_seconds
        .unwrap_or(DEFAULT_RATE_LIMIT_RETRY_SECONDS)
        .min(i64::MAX as u64) as i64;
    ClassifiedQuotaFailure {
        exhaustion: AccountExhaustionRecord::new(
            account_id,
            ExhaustedAccountKind::RateLimited,
            message,
        ),
        effect: QuotaEffect::MarkLimitedUntil(Utc::now() + Duration::seconds(seconds)),
    }
}

fn quota_exhausted_failure(account_id: &str, message: String) -> ClassifiedQuotaFailure {
    ClassifiedQuotaFailure {
        exhaustion: AccountExhaustionRecord::new(
            account_id,
            ExhaustedAccountKind::QuotaExhausted,
            message,
        ),
        effect: QuotaEffect::SetExhausted,
    }
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
