//! Cloudflare recovery state for accounts.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use tokio::sync::RwLock;

const PATH_BLOCK_THRESHOLD: u32 = 3;
const PATH_BLOCK_STALE_AFTER: Duration = Duration::hours(1);

#[derive(Debug, Clone, Copy)]
struct PathBlockState {
    count: u32,
    last_at: DateTime<Utc>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_path_block_should_disable_after_three_recent_failures() {
        let tracker = CloudflarePathBlockTracker::new();
        let now = Utc::now();

        assert_eq!(tracker.record_path_block("acct_1", now).await, 1);
        assert_eq!(
            tracker
                .record_path_block("acct_1", now + Duration::seconds(1))
                .await,
            2
        );
        assert_eq!(
            tracker
                .record_path_block("acct_1", now + Duration::seconds(2))
                .await,
            3
        );

        assert!(
            tracker
                .should_disable("acct_1", now + Duration::seconds(2))
                .await
        );
    }

    #[tokio::test]
    async fn record_path_block_should_ignore_stale_failures() {
        let tracker = CloudflarePathBlockTracker::new();
        let now = Utc::now();

        assert_eq!(tracker.record_path_block("acct_1", now).await, 1);

        assert_eq!(
            tracker
                .record_path_block("acct_1", now + Duration::hours(2))
                .await,
            1
        );
    }

    #[tokio::test]
    async fn reset_should_clear_account_count() {
        let tracker = CloudflarePathBlockTracker::new();
        let now = Utc::now();
        tracker.record_path_block("acct_1", now).await;
        tracker
            .record_path_block("acct_1", now + Duration::seconds(1))
            .await;

        tracker.reset("acct_1").await;

        assert_eq!(tracker.count("acct_1", now + Duration::seconds(2)).await, 0);
    }
}
