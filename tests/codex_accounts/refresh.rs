use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use tokio::time::sleep;

use codex_proxy_rs::{
    codex::accounts::model::{Account, AccountStatus},
    codex::gateway::oauth::{
        RefreshFailure, RefreshPolicy, RefreshScheduler, RefreshTrigger, TokenPair, TokenRefresher,
    },
};

#[tokio::test]
async fn refresh_scheduler_refreshes_before_expiry_and_preserves_refresh_token() {
    let now = Utc::now();
    let mut account = Account::test("acct_1", AccountStatus::Active);
    account.access_token_expires_at = Some(now + ChronoDuration::seconds(60));
    account.refresh_token = Some("rt_keep".to_string());
    let scheduler = RefreshScheduler::new(
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
        },
        StaticRefreshClient {
            result: Ok(TokenPair {
                access_token: "new-access".to_string(),
                refresh_token: None,
            }),
        },
    );

    let refreshed = scheduler
        .refresh_account_at(&account, RefreshTrigger::BeforeExpiry, now)
        .await
        .unwrap();

    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt_keep"));
    assert_eq!(refreshed.status, AccountStatus::Active);
}

#[tokio::test]
async fn refresh_scheduler_refreshes_immediately_after_unauthorized() {
    let now = Utc::now();
    let mut account = Account::test("acct_1", AccountStatus::Active);
    account.access_token_expires_at = Some(now + ChronoDuration::hours(6));
    let scheduler = RefreshScheduler::new(
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
        },
        StaticRefreshClient {
            result: Ok(TokenPair {
                access_token: "new-access-after-401".to_string(),
                refresh_token: Some("rt_rotated".to_string()),
            }),
        },
    );

    let refreshed = scheduler
        .refresh_account_at(&account, RefreshTrigger::Unauthorized, now)
        .await
        .unwrap();

    assert_eq!(refreshed.access_token, "new-access-after-401");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt_rotated"));
}

#[tokio::test]
async fn refresh_scheduler_maps_refresh_failures_to_account_status() {
    let now = Utc::now();
    let account = Account::test("acct_1", AccountStatus::Active);
    let scheduler = RefreshScheduler::new(
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
        },
        StaticRefreshClient {
            result: Err(RefreshFailure::Banned),
        },
    );

    let refreshed = scheduler
        .refresh_account_at(&account, RefreshTrigger::Unauthorized, now)
        .await
        .unwrap();

    assert_eq!(refreshed.status, AccountStatus::Banned);
}

#[tokio::test]
async fn refresh_scheduler_limits_refresh_concurrency() {
    let now = Utc::now();
    let client = CountingRefreshClient::default();
    let scheduler = RefreshScheduler::new(
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
        },
        client.clone(),
    );

    let mut handles = Vec::new();
    for index in 0..5 {
        let mut account = Account::test(&format!("acct_{index}"), AccountStatus::Active);
        account.access_token_expires_at = Some(now + ChronoDuration::seconds(1));
        let scheduler = scheduler.clone();
        handles.push(tokio::spawn(async move {
            scheduler
                .refresh_account_at(&account, RefreshTrigger::BeforeExpiry, now)
                .await
                .unwrap()
        }));
    }

    for handle in handles {
        let refreshed = handle.await.unwrap();
        assert_eq!(refreshed.status, AccountStatus::Active);
    }
    assert_eq!(client.max_seen.load(Ordering::SeqCst), 2);
}

#[derive(Clone)]
struct StaticRefreshClient {
    result: Result<TokenPair, RefreshFailure>,
}

#[async_trait]
impl TokenRefresher for StaticRefreshClient {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.result.clone()
    }
}

#[derive(Clone, Default)]
struct CountingRefreshClient {
    active: Arc<AtomicUsize>,
    max_seen: Arc<AtomicUsize>,
}

#[async_trait]
impl TokenRefresher for CountingRefreshClient {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_seen.fetch_max(current, Ordering::SeqCst);
        sleep(Duration::from_millis(25)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(TokenPair {
            access_token: format!("access-{refresh_token}"),
            refresh_token: None,
        })
    }
}
