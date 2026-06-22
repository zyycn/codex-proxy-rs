//! Cookie 捕获与重放策略。

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use tokio::sync::RwLock;

/// 单个 Cookie 条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieEntry {
    /// Cookie 名称。
    pub name: String,
    /// Cookie 值。
    pub value: String,
}

/// 将 Cookie 条目组装为 HTTP Cookie 头。
pub fn cookie_header(cookies: &[CookieEntry]) -> Option<String> {
    let header = cookies
        .iter()
        .filter(|cookie| !cookie.name.trim().is_empty())
        .map(|cookie| format!("{}={}", cookie.name.trim(), cookie.value))
        .collect::<Vec<_>>()
        .join("; ");
    (!header.is_empty()).then_some(header)
}

const PATH_BLOCK_THRESHOLD: u32 = 3;
const PATH_BLOCK_STALE_AFTER: Duration = Duration::hours(1);
const CHALLENGE_BACKOFF_SECONDS: [i64; 4] = [10, 30, 90, 120];
const CHALLENGE_STALE_AFTER: Duration = Duration::hours(1);

#[derive(Debug, Clone, Copy)]
struct PathBlockState {
    count: u32,
    last_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
struct ChallengeCooldownState {
    challenge_count: u32,
    updated_at: DateTime<Utc>,
}

/// Cloudflare challenge cooldown state after recording one challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudflareChallengeCooldown {
    /// Current non-stale consecutive challenge count.
    pub challenge_count: u32,
    /// Backoff delay selected for this challenge.
    pub delay_seconds: i64,
    /// Time until which this account should be skipped.
    pub cooldown_until: DateTime<Utc>,
    /// Time at which this challenge was recorded.
    pub updated_at: DateTime<Utc>,
}

/// Tracks per-account Cloudflare path-block failures.
#[derive(Debug, Clone, Default)]
pub struct CloudflarePathBlockTracker {
    counts: Arc<RwLock<HashMap<String, PathBlockState>>>,
}

impl CloudflarePathBlockTracker {
    /// Creates an empty path-block tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one path-block failure and returns the current non-stale count.
    pub async fn record_path_block(&self, account_id: &str, now: DateTime<Utc>) -> u32 {
        let mut counts = self.counts.write().await;
        let count = counts
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER)
            .map(|state| state.count.saturating_add(1))
            .unwrap_or(1);
        counts.insert(
            account_id.to_string(),
            PathBlockState {
                count,
                last_at: now,
            },
        );
        count
    }

    /// Clears any tracked path-block failures for an account.
    pub async fn reset(&self, account_id: &str) {
        self.counts.write().await.remove(account_id);
    }

    /// Returns the current non-stale path-block count for an account.
    pub async fn count(&self, account_id: &str, now: DateTime<Utc>) -> u32 {
        self.counts
            .read()
            .await
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER)
            .map(|state| state.count)
            .unwrap_or_default()
    }

    /// Returns whether the account should be disabled at the current count.
    pub async fn should_disable(&self, account_id: &str, now: DateTime<Utc>) -> bool {
        self.count(account_id, now).await >= PATH_BLOCK_THRESHOLD
    }
}

/// Tracks per-account Cloudflare challenge cooldown escalation.
#[derive(Debug, Clone, Default)]
pub struct CloudflareChallengeCooldownTracker {
    states: Arc<RwLock<HashMap<String, ChallengeCooldownState>>>,
}

impl CloudflareChallengeCooldownTracker {
    /// Creates an empty challenge cooldown tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one challenge and returns the current non-stale cooldown state.
    pub async fn record_challenge(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> CloudflareChallengeCooldown {
        let mut states = self.states.write().await;
        let challenge_count = states
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.updated_at) <= CHALLENGE_STALE_AFTER)
            .map(|state| state.challenge_count.saturating_add(1))
            .unwrap_or(1);
        states.insert(
            account_id.to_string(),
            ChallengeCooldownState {
                challenge_count,
                updated_at: now,
            },
        );
        let delay_seconds = challenge_delay_seconds(challenge_count);
        CloudflareChallengeCooldown {
            challenge_count,
            delay_seconds,
            cooldown_until: now + Duration::seconds(delay_seconds),
            updated_at: now,
        }
    }

    /// Clears tracked challenge cooldown state for an account.
    pub async fn reset(&self, account_id: &str) {
        self.states.write().await.remove(account_id);
    }
}

fn challenge_delay_seconds(challenge_count: u32) -> i64 {
    let index = challenge_count
        .saturating_sub(1)
        .min((CHALLENGE_BACKOFF_SECONDS.len() - 1) as u32) as usize;
    CHALLENGE_BACKOFF_SECONDS[index]
}
