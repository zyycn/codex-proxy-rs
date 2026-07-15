//! Cloudflare challenge、路径封禁与 Cookie 生命周期的唯一 feature owner。

use chrono::Utc;

use crate::{
    dispatch::{
        failure::exhaustion::{AccountExhaustionRecord, ExhaustedAccountKind},
        lifecycle::contract::{
            AttemptDecision, AttemptObservation, AttemptObservationKind, AttemptReturnKind,
        },
        lifecycle::stream::StreamTerminal,
    },
    fleet::{
        account::AccountStatus,
        cookies::{CloudflareChallengeCooldownTracker, CloudflarePathBlockTracker, PgCookieStore},
        pool::AccountPoolService,
    },
    upstream::openai::failure::UpstreamFailureFacts,
};

pub(super) struct CloudflareController;

pub(super) struct StreamExit<'a> {
    pub recovery: &'a CloudflareRecovery,
    pub account_id: &'a str,
    pub set_cookie_headers: &'a [String],
    pub terminal: &'a StreamTerminal,
}

#[derive(Clone, Copy)]
enum CloudflareFailureKind {
    Challenge,
    PathBlocked,
}

pub(super) struct CloudflareFailure {
    kind: CloudflareFailureKind,
    exhaustion: AccountExhaustionRecord,
}

impl CloudflareController {
    pub(super) fn classify(observation: &AttemptObservation) -> Option<CloudflareFailure> {
        classify(observation)
    }

    pub(super) async fn apply_effect(
        recovery: &CloudflareRecovery,
        account_pool: &AccountPoolService,
        failure: &CloudflareFailure,
    ) {
        match failure.kind {
            CloudflareFailureKind::Challenge => {
                let Some(account_id) = failure.exhaustion.account_id.as_deref() else {
                    return;
                };
                recovery.apply_challenge(account_pool, account_id).await;
            }
            CloudflareFailureKind::PathBlocked => {
                let Some(account_id) = failure.exhaustion.account_id.as_deref() else {
                    return;
                };
                recovery.apply_path_block(account_pool, account_id).await;
            }
        }
    }

    pub(super) fn decision(
        observation: &AttemptObservation,
        failure: CloudflareFailure,
    ) -> AttemptDecision {
        if observation.routing.can_retry_next_candidate {
            return AttemptDecision::RetryNextCandidate {
                exhaustion: Some(failure.exhaustion),
                on_exhaustion: None,
            };
        }
        AttemptDecision::Return(AttemptReturnKind::Observed)
    }

    pub(super) async fn leave_complete(recovery: &CloudflareRecovery, account_id: &str) {
        recovery.reset_account_recovery(account_id).await;
    }

    pub(super) async fn leave_stream(exit: StreamExit<'_>) {
        exit.recovery
            .capture_set_cookie_headers(exit.account_id, exit.set_cookie_headers)
            .await;
        if matches!(
            exit.terminal,
            StreamTerminal::Completed { .. } | StreamTerminal::Incomplete { .. }
        ) {
            exit.recovery.reset_account_recovery(exit.account_id).await;
        }
    }
}

fn classify(observation: &AttemptObservation) -> Option<CloudflareFailure> {
    let account_id = observation.account.as_ref()?.id.as_str();
    let AttemptObservationKind::UpstreamFailure(facts) = &observation.kind else {
        return None;
    };
    classify_upstream(account_id, facts)
}

fn classify_upstream(account_id: &str, facts: &UpstreamFailureFacts) -> Option<CloudflareFailure> {
    if facts.status_code == Some(403) && is_challenge(&facts.body) {
        return Some(CloudflareFailure {
            kind: CloudflareFailureKind::Challenge,
            exhaustion: AccountExhaustionRecord::new(
                account_id,
                ExhaustedAccountKind::CloudflareChallenge,
                "Upstream blocked the request (Cloudflare challenge)",
            ),
        });
    }
    if facts.status_code == Some(404) && facts.body.trim().is_empty() {
        return Some(CloudflareFailure {
            kind: CloudflareFailureKind::PathBlocked,
            exhaustion: AccountExhaustionRecord::new(
                account_id,
                ExhaustedAccountKind::CloudflarePathBlocked,
                "Upstream blocked the request (Cloudflare path-block)",
            ),
        });
    }
    None
}

fn is_challenge(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("cf-mitigated")
        || value.contains("cf-chl-bypass")
        || value.contains("_cf_chl")
        || value.contains("cf_chl")
        || value.contains("attention required")
        || value.contains("just a moment")
}

#[derive(Clone)]
pub struct CloudflareRecovery {
    path_block_tracker: CloudflarePathBlockTracker,
    challenge_tracker: CloudflareChallengeCooldownTracker,
    cookie_store: PgCookieStore,
}

impl CloudflareRecovery {
    pub fn new(cookie_store: PgCookieStore) -> Self {
        Self {
            path_block_tracker: CloudflarePathBlockTracker::default(),
            challenge_tracker: CloudflareChallengeCooldownTracker::default(),
            cookie_store,
        }
    }

    pub async fn cookie_header_for_request(&self, account_id: &str, path: &str) -> Option<String> {
        self.cookie_store
            .cookie_header_for_request(account_id, "chatgpt.com", path)
            .await
            .ok()?
    }

    pub async fn capture_set_cookie_headers(&self, account_id: &str, headers: &[String]) {
        for header in headers {
            if let Err(error) = self
                .cookie_store
                .capture_set_cookie(account_id, header)
                .await
            {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "Failed to persist upstream set-cookie header"
                );
            }
        }
    }

    pub async fn apply_challenge(&self, account_pool: &AccountPoolService, account_id: &str) {
        let now = Utc::now();
        let cooldown = self
            .challenge_tracker
            .record_challenge(account_id, now)
            .await;
        account_pool
            .set_cloudflare_cooldown_until(account_id, cooldown.cooldown_until)
            .await;
        if let Err(error) = self
            .cookie_store
            .expire_account_cookies_at(account_id, cooldown.cooldown_until)
            .await
        {
            tracing::warn!(
                account_id,
                error = %error,
                "Failed to persist Cloudflare challenge cookie cleanup deadline"
            );
        }
    }

    pub async fn apply_path_block(&self, account_pool: &AccountPoolService, account_id: &str) {
        self.delete_account_cookies(account_id, "Cloudflare path-block")
            .await;
        let now = Utc::now();
        self.path_block_tracker
            .record_path_block(account_id, now)
            .await;
        if self
            .path_block_tracker
            .should_disable(account_id, now)
            .await
        {
            account_pool
                .set_status(account_id, AccountStatus::Disabled)
                .await;
        }
    }

    pub async fn reset_account_recovery(&self, account_id: &str) {
        self.path_block_tracker.reset(account_id).await;
        self.challenge_tracker.reset(account_id).await;
    }

    async fn delete_account_cookies(&self, account_id: &str, reason: &str) {
        delete_account_cookies(&self.cookie_store, account_id, reason).await;
    }
}

async fn delete_account_cookies(cookie_store: &PgCookieStore, account_id: &str, reason: &str) {
    if let Err(error) = cookie_store.delete_account_cookies(account_id).await {
        tracing::warn!(
            account_id,
            reason,
            error = %error,
            "Failed to delete account cookies after Cloudflare recovery signal"
        );
    }
}
