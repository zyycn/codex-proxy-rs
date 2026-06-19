use super::*;

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
