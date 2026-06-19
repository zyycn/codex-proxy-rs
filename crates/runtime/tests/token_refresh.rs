use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, TimeZone, Utc};
use codex_proxy_adapters::sqlite::{
    accounts::{NewAccount, SqliteAccountStore},
    refresh_leases::SqliteRefreshLeaseStore,
};
use codex_proxy_core::{
    accounts::model::AccountStatus,
    auth::{
        oauth::{RefreshFailure, RefreshPolicy, TokenPair},
        ports::TokenRefresher,
    },
};
use codex_proxy_platform::{crypto::SecretBox, storage::connect_sqlite};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use tokio::{
    sync::{Mutex, Notify},
    time::{sleep, timeout},
};

#[tokio::test]
async fn token_refresh_task_should_persist_refreshed_access_token_and_keep_refresh_token() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([11u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 8, 0, 0).unwrap();
    let new_expires_at = now + Duration::hours(1);
    let new_access_token = test_jwt(new_expires_at.timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh".to_string(),
            email: Some("user@example.com".to_string()),
            account_id: Some("chatgpt-account".to_string()),
            user_id: Some("user-id".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new("refresh-old".to_string().into())),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let refresher = StaticTokenRefresher {
        response: Arc::new(Mutex::new(Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }))),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    );

    let summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("refresh should succeed");
    let stored = store
        .get("acct-refresh")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(
        (
            summary.refreshed,
            stored.access_token.expose_secret(),
            stored
                .refresh_token
                .as_ref()
                .map(ExposeSecret::expose_secret),
            stored.access_token_expires_at,
            stored.status,
        ),
        (
            1,
            new_access_token.as_str(),
            Some("refresh-old"),
            Some(new_expires_at),
            AccountStatus::Active,
        )
    );
}

#[tokio::test]
async fn token_refresh_task_should_skip_account_when_refresh_lease_is_held() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-lease.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool.clone(), SecretBox::new([12u8; 32]));
    let leases = SqliteRefreshLeaseStore::new(pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 8, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::seconds(30)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-lease-held".to_string(),
            email: Some("lease@example.com".to_string()),
            account_id: Some("chatgpt-lease".to_string()),
            user_id: Some("user-lease".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: Some(SecretString::new("refresh-held".to_string().into())),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    assert!(leases
        .try_acquire(
            "acct-lease-held",
            "external-owner",
            now + Duration::minutes(5),
            now,
        )
        .await
        .expect("external owner should acquire lease"));
    let refresher = CountingTokenRefresher::default();
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    )
    .with_refresh_lease_store(leases);

    let summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("refresh scan should skip lease-held account");
    let stored = store
        .get("acct-lease-held")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.refreshed, 0);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn token_refresh_task_should_mark_due_account_refreshing_before_refresher_call() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-status.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([13u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 9, 0, 0).unwrap();
    let new_expires_at = now + Duration::hours(1);
    let new_access_token = test_jwt(new_expires_at.timestamp());
    store
        .insert(NewAccount {
            id: "acct-refreshing-marker".to_string(),
            email: Some("marker@example.com".to_string()),
            account_id: Some("chatgpt-marker".to_string()),
            user_id: Some("user-marker".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new("refresh-marker".to_string().into())),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let observed_statuses = Arc::new(Mutex::new(Vec::new()));
    let refresher = StoreObservingTokenRefresher {
        store: store.clone(),
        account_id: "acct-refreshing-marker".to_string(),
        observed_statuses: observed_statuses.clone(),
        response: Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    );

    let summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("refresh should succeed");
    let stored = store
        .get("acct-refreshing-marker")
        .await
        .expect("account should load")
        .expect("account should exist");
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(summary.refreshed, 1);
    assert_eq!(observed_statuses, [AccountStatus::Refreshing]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_recover_refreshing_account_after_restart() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-recovery.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([14u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 10, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    let new_access_token = test_jwt((now + Duration::hours(2)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refreshing-recovery".to_string(),
            email: Some("recovery@example.com".to_string()),
            account_id: Some("chatgpt-recovery".to_string()),
            user_id: Some("user-recovery".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.into()),
            refresh_token: Some(SecretString::new("refresh-recovery".to_string().into())),
            access_token_expires_at: Some(now + Duration::hours(1)),
            status: AccountStatus::Refreshing,
        })
        .await
        .expect("account should be inserted");
    let refresher = StaticTokenRefresher {
        response: Arc::new(Mutex::new(Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }))),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    );

    let summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("refreshing account should recover");
    let stored = store
        .get("acct-refreshing-recovery")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(summary.refreshed, 1);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_restore_active_status_after_transport_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-transport-failure.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([15u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 11, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::seconds(30)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-transport".to_string(),
            email: Some("transport@example.com".to_string()),
            account_id: Some("chatgpt-transport".to_string()),
            user_id: Some("user-transport".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: Some(SecretString::new("refresh-transport".to_string().into())),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let observed_statuses = Arc::new(Mutex::new(Vec::new()));
    let refresher = StoreObservingTokenRefresher {
        store: store.clone(),
        account_id: "acct-refresh-transport".to_string(),
        observed_statuses: observed_statuses.clone(),
        response: Err(RefreshFailure::Transport),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("transport failure should be summarized");
    let stored = store
        .get("acct-refresh-transport")
        .await
        .expect("account should load")
        .expect("account should exist");
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(summary.failed, 1);
    assert_eq!(observed_statuses, [AccountStatus::Refreshing; 5]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_skip_duplicate_in_flight_refresh() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-in-flight.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([18u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 14, 0, 0).unwrap();
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-in-flight".to_string(),
            email: Some("in-flight@example.com".to_string()),
            account_id: Some("chatgpt-in-flight".to_string()),
            user_id: Some("user-in-flight".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new("refresh-in-flight".to_string().into())),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let refresher = BlockingTokenRefresher {
        calls: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        response: Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }),
    };
    let task = Arc::new(
        codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
            store.clone(),
            RefreshPolicy {
                refresh_margin_seconds: 300,
                refresh_concurrency: 1,
            },
            refresher.clone(),
        ),
    );

    let first = {
        let task = task.clone();
        tokio::spawn(async move { task.refresh_due_accounts_once_at(now).await })
    };
    refresher.started.notified().await;

    let second = {
        let task = task.clone();
        tokio::spawn(async move { task.refresh_due_accounts_once_at(now).await })
    };
    let second = match timeout(StdDuration::from_millis(100), second).await {
        Ok(result) => result.expect("second scan should join"),
        Err(_) => {
            refresher.release.notify_waiters();
            let _ = first.await;
            panic!("second scan should skip instead of waiting on duplicate refresh");
        }
    }
    .expect("second scan should succeed");

    refresher.release.notify_waiters();
    let first = first
        .await
        .expect("first scan should join")
        .expect("first scan should succeed");
    let stored = store
        .get("acct-refresh-in-flight")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(first.refreshed, 1);
    assert_eq!(second.skipped, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_persist_success_after_transient_transport_retry() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-transient-success.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([16u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 12, 0, 0).unwrap();
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-transient-success".to_string(),
            email: Some("transient@example.com".to_string()),
            account_id: Some("chatgpt-transient".to_string()),
            user_id: Some("user-transient".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new("refresh-transient".to_string().into())),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let observed_statuses = Arc::new(Mutex::new(Vec::new()));
    let refresher = SequenceTokenRefresher {
        store: store.clone(),
        account_id: "acct-refresh-transient-success".to_string(),
        observed_statuses: observed_statuses.clone(),
        responses: Arc::new(Mutex::new(VecDeque::from(vec![
            Err(RefreshFailure::Transport),
            Ok(TokenPair {
                access_token: new_access_token.clone(),
                refresh_token: None,
            }),
        ]))),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("retry should eventually refresh");
    let stored = store
        .get("acct-refresh-transient-success")
        .await
        .expect("account should load")
        .expect("account should exist");
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(summary.refreshed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(observed_statuses, [AccountStatus::Refreshing; 2]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_delay_recovery_after_retry_exhaustion() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-delayed-recovery.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([19u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 15, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::seconds(30)).timestamp());
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-delayed-recovery".to_string(),
            email: Some("delayed-recovery@example.com".to_string()),
            account_id: Some("chatgpt-delayed-recovery".to_string()),
            user_id: Some("user-delayed-recovery".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: Some(SecretString::new(
                "refresh-delayed-recovery".to_string().into(),
            )),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let mut responses = Vec::new();
    responses.extend((0..5).map(|_| Err(RefreshFailure::Transport)));
    responses.push(Ok(TokenPair {
        access_token: new_access_token.clone(),
        refresh_token: None,
    }));
    let observed_statuses = Arc::new(Mutex::new(Vec::new()));
    let refresher = SequenceTokenRefresher {
        store: store.clone(),
        account_id: "acct-refresh-delayed-recovery".to_string(),
        observed_statuses: observed_statuses.clone(),
        responses: Arc::new(Mutex::new(VecDeque::from(responses))),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let failed = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("first scan should summarize retry exhaustion");
    let delayed = task
        .refresh_due_accounts_once_at(now + Duration::minutes(5))
        .await
        .expect("recovery window should skip refresh");
    let refreshed = task
        .refresh_due_accounts_once_at(now + Duration::minutes(10) + Duration::seconds(1))
        .await
        .expect("recovery window should allow refresh");
    let stored = store
        .get("acct-refresh-delayed-recovery")
        .await
        .expect("account should load")
        .expect("account should exist");
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(failed.failed, 1);
    assert_eq!(delayed.skipped, 1);
    assert_eq!(refreshed.refreshed, 1);
    assert_eq!(observed_statuses, [AccountStatus::Refreshing; 6]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_schedule_future_per_account_timer_without_refreshing_early() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-future-timer.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([20u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 16, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::minutes(10)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-future-timer".to_string(),
            email: Some("future-timer@example.com".to_string()),
            account_id: Some("chatgpt-future-timer".to_string()),
            user_id: Some("user-future-timer".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: Some(SecretString::new("refresh-future-timer".to_string().into())),
            access_token_expires_at: Some(now + Duration::minutes(10)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let refresher = CountingTokenRefresher::default();
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store,
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    );

    let summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("timer scheduling should succeed");
    tokio::task::yield_now().await;

    assert_eq!(summary.scheduled, 1);
    assert_eq!(task.scheduled_timer_count().await, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn token_refresh_task_should_fire_per_account_timer_at_refresh_time() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-per-account-timer.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([21u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 17, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::seconds(1)).timestamp());
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-timer".to_string(),
            email: Some("timer@example.com".to_string()),
            account_id: Some("chatgpt-timer".to_string()),
            user_id: Some("user-timer".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: Some(SecretString::new("refresh-timer".to_string().into())),
            access_token_expires_at: Some(now + Duration::seconds(1)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let refresher = NotifyingTokenRefresher {
        calls: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Notify::new()),
        response: Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 1,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let started = refresher.started.notified();
    tokio::pin!(started);
    let summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("timer scheduling should succeed");
    timeout(StdDuration::from_secs(2), &mut started)
        .await
        .expect("scheduled timer should trigger refresh");
    tokio::task::yield_now().await;
    let stored = store
        .get("acct-refresh-timer")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(summary.immediate, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(task.scheduled_timer_count().await, 1);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_reschedule_next_timer_after_scheduled_refresh() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-next-timer.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([22u8; 32]));
    let now = Utc::now();
    let old_expires_at = now + Duration::milliseconds(50);
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-next-timer".to_string(),
            email: Some("next-timer@example.com".to_string()),
            account_id: Some("chatgpt-next-timer".to_string()),
            user_id: Some("user-next-timer".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(test_jwt(old_expires_at.timestamp()).into()),
            refresh_token: Some(SecretString::new("refresh-next-timer".to_string().into())),
            access_token_expires_at: Some(old_expires_at),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let refresher = NotifyingTokenRefresher {
        calls: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Notify::new()),
        response: Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 0,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let started = refresher.started.notified();
    tokio::pin!(started);
    let summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("timer scheduling should succeed");
    timeout(StdDuration::from_secs(2), &mut started)
        .await
        .expect("scheduled timer should trigger refresh");

    let mut stored = store
        .get("acct-refresh-next-timer")
        .await
        .expect("account should load")
        .expect("account should exist");
    for _ in 0..50 {
        if stored.access_token.expose_secret() == new_access_token.as_str() {
            break;
        }
        sleep(StdDuration::from_millis(20)).await;
        stored = store
            .get("acct-refresh-next-timer")
            .await
            .expect("account should load")
            .expect("account should exist");
    }

    assert_eq!(summary.scheduled, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    assert_eq!(task.scheduled_timer_count().await, 1);
}

#[tokio::test]
async fn token_refresh_task_should_confirm_invalid_grant_before_expiring_account() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-permanent-failure.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([17u8; 32]));
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 13, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::seconds(30)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-invalid-grant".to_string(),
            email: Some("invalid-grant@example.com".to_string()),
            account_id: Some("chatgpt-invalid-grant".to_string()),
            user_id: Some("user-invalid-grant".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: Some(SecretString::new(
                "refresh-invalid-grant".to_string().into(),
            )),
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let observed_statuses = Arc::new(Mutex::new(Vec::new()));
    let refresher = SequenceTokenRefresher {
        store: store.clone(),
        account_id: "acct-refresh-invalid-grant".to_string(),
        observed_statuses: observed_statuses.clone(),
        responses: Arc::new(Mutex::new(VecDeque::from(vec![
            Err(RefreshFailure::InvalidGrant),
            Err(RefreshFailure::InvalidGrant),
        ]))),
    };
    let task = codex_proxy_runtime::tasks::token_refresh::TokenRefreshTask::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("confirmed permanent failure should update status");
    let stored = store
        .get("acct-refresh-invalid-grant")
        .await
        .expect("account should load")
        .expect("account should exist");
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(summary.status_updated, 1);
    assert_eq!(observed_statuses, [AccountStatus::Refreshing; 2]);
    assert_eq!(stored.status, AccountStatus::Expired);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
}

#[derive(Clone)]
struct StaticTokenRefresher {
    response: Arc<Mutex<Result<TokenPair, RefreshFailure>>>,
}

#[async_trait]
impl TokenRefresher for StaticTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.response.lock().await.clone()
    }
}

#[derive(Clone, Default)]
struct CountingTokenRefresher {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TokenRefresher for CountingTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(TokenPair {
            access_token: "new-access".to_string(),
            refresh_token: None,
        })
    }
}

#[derive(Clone)]
struct BlockingTokenRefresher {
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    release: Arc<Notify>,
    response: Result<TokenPair, RefreshFailure>,
}

#[async_trait]
impl TokenRefresher for BlockingTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_waiters();
        self.release.notified().await;
        self.response.clone()
    }
}

#[derive(Clone)]
struct NotifyingTokenRefresher {
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    response: Result<TokenPair, RefreshFailure>,
}

#[async_trait]
impl TokenRefresher for NotifyingTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_waiters();
        self.response.clone()
    }
}

#[derive(Clone)]
struct StoreObservingTokenRefresher {
    store: SqliteAccountStore,
    account_id: String,
    observed_statuses: Arc<Mutex<Vec<AccountStatus>>>,
    response: Result<TokenPair, RefreshFailure>,
}

#[async_trait]
impl TokenRefresher for StoreObservingTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        let status = self
            .store
            .get(&self.account_id)
            .await
            .expect("account should load")
            .expect("account should exist")
            .status;
        self.observed_statuses.lock().await.push(status);
        self.response.clone()
    }
}

#[derive(Clone)]
struct SequenceTokenRefresher {
    store: SqliteAccountStore,
    account_id: String,
    observed_statuses: Arc<Mutex<Vec<AccountStatus>>>,
    responses: Arc<Mutex<VecDeque<Result<TokenPair, RefreshFailure>>>>,
}

#[async_trait]
impl TokenRefresher for SequenceTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        let status = self
            .store
            .get(&self.account_id)
            .await
            .expect("account should load")
            .expect("account should exist")
            .status;
        self.observed_statuses.lock().await.push(status);
        self.responses
            .lock()
            .await
            .pop_front()
            .expect("refresh response should be configured")
    }
}

fn test_jwt(exp: i64) -> String {
    let header = json!({"alg": "none", "typ": "JWT"});
    let payload = json!({ "exp": exp });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload))
}

fn jwt_part(value: &serde_json::Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("json should encode"))
}
