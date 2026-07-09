use super::*;

#[tokio::test]
async fn token_refresh_task_should_skip_account_when_refresh_lease_is_held() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-lease.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool.clone());
    let leases = RefreshLeaseStore::new(pool.clone());
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
            added_at: None,
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
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
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
    let store = SqliteAccountStore::new(pool);
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
            added_at: None,
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
        codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
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
    let Ok(result) = timeout(StdDuration::from_millis(100), second).await else {
        refresher.release.notify_waiters();
        let _ = first.await;
        panic!("second scan should skip instead of waiting on duplicate refresh");
    };
    let second = result
        .expect("second scan should join")
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
    let store = SqliteAccountStore::new(pool);
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
            added_at: None,
            access_token_expires_at: Some(now + Duration::minutes(10)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let refresher = CountingTokenRefresher::default();
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
        store.clone(),
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
    let stored = store
        .get("acct-refresh-future-timer")
        .await
        .expect("account should load")
        .expect("account should exist");
    tokio::task::yield_now().await;

    assert_eq!(summary.scheduled, 1);
    assert_eq!(task.scheduled_timer_count().await, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
    let scheduled_at = stored
        .next_refresh_at
        .expect("future refresh timer should persist next_refresh_at");
    let exact_margin_at = now + Duration::minutes(5);
    assert_ne!(scheduled_at, exact_margin_at);
    assert!(scheduled_at >= now + Duration::seconds(255));
    assert!(scheduled_at <= now + Duration::seconds(345));
}

#[tokio::test]
async fn token_refresh_task_should_refresh_immediately_after_unauthorized_even_with_future_timer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-unauthorized-now.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool);
    let now = Utc::now();
    let old_access_token = test_jwt((now + Duration::minutes(10)).timestamp());
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-unauthorized-now".to_string(),
            email: Some("unauthorized-now@example.com".to_string()),
            account_id: Some("chatgpt-unauthorized-now".to_string()),
            user_id: Some("user-unauthorized-now".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.into()),
            refresh_token: Some(SecretString::new(
                "refresh-unauthorized-now".to_string().into(),
            )),
            added_at: None,
            access_token_expires_at: Some(now + Duration::minutes(10)),
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
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);
    task.schedule_account_timers_once_at(now)
        .await
        .expect("future timer should be scheduled");
    assert_eq!(task.scheduled_timer_count().await, 1);
    assert!(store
        .set_status("acct-refresh-unauthorized-now", AccountStatus::Expired)
        .await
        .expect("expired status should persist"));

    task.trigger_account_refresh_now("acct-refresh-unauthorized-now")
        .await
        .expect("unauthorized refresh should run immediately");
    let stored = store
        .get("acct-refresh-unauthorized-now")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    assert!(stored.next_refresh_at.is_some());
    assert_eq!(task.scheduled_timer_count().await, 1);
}

#[tokio::test]
async fn token_refresh_task_should_skip_unauthorized_refresh_without_refresh_token() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-unauthorized-no-rt.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool);
    let now = Utc::now();
    let old_access_token = test_jwt((now + Duration::minutes(10)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-unauthorized-no-rt".to_string(),
            email: Some("unauthorized-no-rt@example.com".to_string()),
            account_id: Some("chatgpt-unauthorized-no-rt".to_string()),
            user_id: Some("user-unauthorized-no-rt".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: None,
            added_at: None,
            access_token_expires_at: Some(now + Duration::minutes(10)),
            status: AccountStatus::Expired,
        })
        .await
        .expect("account should be inserted");
    let refresher = CountingTokenRefresher::default();
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    );

    task.trigger_account_refresh_now("acct-refresh-unauthorized-no-rt")
        .await
        .expect("unauthorized refresh should no-op without refresh token");
    let stored = store
        .get("acct-refresh-unauthorized-no-rt")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(stored.status, AccountStatus::Expired);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_clear_stale_refresh_time_without_refresh_token() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-no-rt-clear-next.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool);
    let now = Utc::now();
    let old_access_token = test_jwt((now + Duration::minutes(10)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-no-rt-clear-next".to_string(),
            email: Some("no-rt-clear-next@example.com".to_string()),
            account_id: Some("chatgpt-no-rt-clear-next".to_string()),
            user_id: Some("user-no-rt-clear-next".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.clone().into()),
            refresh_token: None,
            added_at: None,
            access_token_expires_at: Some(now + Duration::minutes(10)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    store
        .set_next_refresh_at(
            "acct-refresh-no-rt-clear-next",
            Some(now + Duration::minutes(5)),
        )
        .await
        .expect("stale future refresh time should persist");
    let refresher = CountingTokenRefresher::default();
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    );

    let timer_summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("timer scheduling should skip account without refresh token");
    let after_timer_scan = store
        .get("acct-refresh-no-rt-clear-next")
        .await
        .expect("account should load")
        .expect("account should exist");
    store
        .set_next_refresh_at(
            "acct-refresh-no-rt-clear-next",
            Some(now - Duration::minutes(1)),
        )
        .await
        .expect("stale due refresh time should persist");

    let due_summary = task
        .refresh_due_accounts_once_at(now)
        .await
        .expect("due refresh scan should skip account without refresh token");
    let stored = store
        .get("acct-refresh-no-rt-clear-next")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(timer_summary.skipped, 1);
    assert_eq!(due_summary.skipped, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
    assert!(after_timer_scan.next_refresh_at.is_none());
    assert!(stored.next_refresh_at.is_none());
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_fire_per_account_timer_at_refresh_time() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-per-account-timer.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool);
    let now = Utc::now() + Duration::minutes(5);
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
            added_at: None,
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
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
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
async fn token_refresh_task_should_fire_quota_exhausted_timer_without_clearing_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir
        .path()
        .join("token-refresh-quota-exhausted-timer.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool);
    let now = Utc::now();
    let old_expires_at = now + Duration::milliseconds(50);
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-quota-exhausted-timer".to_string(),
            email: Some("quota-exhausted-timer@example.com".to_string()),
            account_id: Some("chatgpt-quota-exhausted-timer".to_string()),
            user_id: Some("user-quota-exhausted-timer".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(test_jwt(old_expires_at.timestamp()).into()),
            refresh_token: Some(SecretString::new(
                "refresh-quota-exhausted-timer".to_string().into(),
            )),
            added_at: None,
            access_token_expires_at: Some(old_expires_at),
            status: AccountStatus::QuotaExhausted,
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
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
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
        .expect("quota-exhausted timer should be scheduled");
    timeout(StdDuration::from_secs(2), &mut started)
        .await
        .expect("quota-exhausted timer should trigger refresh");

    let mut stored = store
        .get("acct-refresh-quota-exhausted-timer")
        .await
        .expect("account should load")
        .expect("account should exist");
    for _ in 0..50 {
        if stored.access_token.expose_secret() == new_access_token.as_str() {
            break;
        }
        sleep(StdDuration::from_millis(20)).await;
        stored = store
            .get("acct-refresh-quota-exhausted-timer")
            .await
            .expect("account should load")
            .expect("account should exist");
    }

    assert_eq!(summary.scheduled, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(stored.status, AccountStatus::QuotaExhausted);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    assert!(stored.next_refresh_at.is_some());
}

#[tokio::test]
async fn token_refresh_task_should_reschedule_next_timer_after_scheduled_refresh() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-next-timer.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool);
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
            added_at: None,
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
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
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
