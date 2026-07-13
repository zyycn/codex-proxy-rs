//! Cloudflare challenge and path-block recovery shared by dispatch routes.

use chrono::Utc;

use crate::{
    fleet::{
        account::AccountStatus,
        cookies::{CloudflareChallengeCooldownTracker, CloudflarePathBlockTracker, PgCookieStore},
        pool::AccountPoolService,
    },
    upstream::openai::transport::CodexClientError,
};

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

pub fn is_cloudflare_challenge_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.as_u16() == 403 && is_cloudflare_challenge_signal(body)
    )
}

pub fn is_cloudflare_path_block_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.as_u16() == 404 && body.trim().is_empty()
    )
}

pub fn cloudflare_challenge_error_message() -> &'static str {
    "Upstream blocked the request (Cloudflare challenge)"
}

pub fn cloudflare_path_block_error_message() -> &'static str {
    "Upstream blocked the request (Cloudflare path-block)"
}

fn is_cloudflare_challenge_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("cf-mitigated")
        || value.contains("cf-chl-bypass")
        || value.contains("_cf_chl")
        || value.contains("cf_chl")
        || value.contains("attention required")
        || value.contains("just a moment")
}
