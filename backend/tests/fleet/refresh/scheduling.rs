use super::*;

#[tokio::test]
async fn token_refresh_task_should_skip_account_when_refresh_lease_is_held() {
    let (pool, _guard) = init_test_db("token-refresh-lease").await;
    let store = PgAccountStore::new(pool.clone());
    let leases = RedisRefreshLeaseStore::new(create_test_redis("token-refresh-lease").await);
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
    assert!(
        leases
            .try_acquire(
                "acct-lease-held",
                "external-owner",
                now + Duration::minutes(5),
                now,
            )
            .await
            .expect("external owner should acquire lease")
    );
    let refresher = CountingTokenRefresher::default();
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    )
    .with_refresh_lease_store(leases);

    let timer_summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("refresh timer should be scheduled");
    wait_for_no_scheduled_timers(&task).await;
    task.shutdown().await;
    let stored = store
        .get("acct-lease-held")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(timer_summary.immediate, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn token_refresh_task_should_skip_duplicate_in_flight_refresh() {
    let (pool, _guard) = init_test_db("token-refresh-in-flight").await;
    let store = PgAccountStore::new(pool);
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    );

    let started = refresher.started.notified();
    tokio::pin!(started);
    let first = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("first refresh timer should be scheduled");
    timeout(StdDuration::from_secs(2), &mut started)
        .await
        .expect("first refresh should start");

    let Ok(second) = timeout(
        StdDuration::from_millis(100),
        task.schedule_account_timers_once_at(now),
    )
    .await
    else {
        refresher.release.notify_waiters();
        task.shutdown().await;
        panic!("second timer scan should not wait on the in-flight refresh");
    };
    let second = second.expect("second refresh timer should be scheduled");
    wait_for_no_scheduled_timers(&task).await;

    refresher.release.notify_waiters();
    task.shutdown().await;
    let stored = wait_for_account(&store, "acct-refresh-in-flight", |account| {
        account.access_token.expose_secret() == new_access_token.as_str()
    })
    .await;

    assert_eq!(first.immediate, 1);
    assert_eq!(second.immediate, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_shutdown_should_wait_for_in_flight_refresh() {
    let (pool, _guard) = init_test_db("token-refresh-shutdown").await;
    let store = PgAccountStore::new(pool);
    let now = Utc::now();
    let new_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-shutdown".to_string(),
            email: Some("shutdown@example.com".to_string()),
            account_id: Some("chatgpt-shutdown".to_string()),
            user_id: Some("user-shutdown".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new("refresh-shutdown".to_string().into())),
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
    let task = Arc::new(codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher.clone(),
    ));
    let started = refresher.started.notified();
    tokio::pin!(started);
    task.schedule_account_timers_once_at(now)
        .await
        .expect("refresh timer should be scheduled");
    timeout(StdDuration::from_secs(2), &mut started)
        .await
        .expect("refresh should start");

    let shutdown_task = task.clone();
    let shutdown = tokio::spawn(async move { shutdown_task.shutdown().await });
    tokio::pin!(shutdown);
    assert!(
        timeout(StdDuration::from_millis(100), &mut shutdown)
            .await
            .is_err()
    );

    refresher.release.notify_waiters();
    timeout(StdDuration::from_secs(2), &mut shutdown)
        .await
        .expect("shutdown should finish after refresh")
        .expect("shutdown task should join");
    let stored = wait_for_account(&store, "acct-refresh-shutdown", |account| {
        account.access_token.expose_secret() == new_access_token.as_str()
    })
    .await;

    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(stored.access_token.expose_secret(), new_access_token);
}

#[tokio::test]
async fn token_refresh_task_should_schedule_future_per_account_timer_without_refreshing_early() {
    let (pool, _guard) = init_test_db("token-refresh-future-timer").await;
    let store = PgAccountStore::new(pool);
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
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
    task.shutdown().await;
}

#[tokio::test]
async fn token_refresh_task_should_cancel_future_timer_without_refreshing_expired_account() {
    let (pool, _guard) = init_test_db("token-refresh-unauthorized-now").await;
    let store = PgAccountStore::new(pool);
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
            access_token: SecretString::new(old_access_token.clone().into()),
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
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
    assert!(
        store
            .set_status("acct-refresh-unauthorized-now", AccountStatus::Expired)
            .await
            .expect("expired status should persist")
    );

    task.schedule_account_timers_once_at(now)
        .await
        .expect("expired account schedule should be cleared");
    let stored = store
        .get("acct-refresh-unauthorized-now")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(stored.status, AccountStatus::Expired);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
    assert!(stored.next_refresh_at.is_none());
    assert_eq!(task.scheduled_timer_count().await, 0);
    task.shutdown().await;
}

#[tokio::test]
async fn token_refresh_task_should_clear_stale_refresh_time_without_refresh_token() {
    let (pool, _guard) = init_test_db("token-refresh-no-rt-clear-next").await;
    let store = PgAccountStore::new(pool);
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
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
        .schedule_account_timers_once_at(now)
        .await
        .expect("due timer scan should skip account without refresh token");
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
    task.shutdown().await;
}

#[tokio::test]
async fn token_refresh_task_should_fire_per_account_timer_at_refresh_time() {
    let (pool, _guard) = init_test_db("token-refresh-per-account-timer").await;
    let store = PgAccountStore::new(pool);
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
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
    let stored = wait_for_account(&store, "acct-refresh-timer", |account| {
        account.access_token.expose_secret() == new_access_token.as_str()
    })
    .await;

    assert_eq!(summary.immediate, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(task.scheduled_timer_count().await, 1);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    task.shutdown().await;
}

#[tokio::test]
async fn token_refresh_task_should_fire_quota_exhausted_timer_without_clearing_status() {
    let (pool, _guard) = init_test_db("token-refresh-quota-exhausted-timer").await;
    let store = PgAccountStore::new(pool);
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
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

    let stored = wait_for_account(&store, "acct-refresh-quota-exhausted-timer", |account| {
        account.access_token.expose_secret() == new_access_token.as_str()
    })
    .await;

    assert_eq!(summary.scheduled, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(stored.status, AccountStatus::QuotaExhausted);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    assert!(stored.next_refresh_at.is_some());
    task.shutdown().await;
}

#[tokio::test]
async fn token_refresh_task_should_reschedule_next_timer_after_scheduled_refresh() {
    let (pool, _guard) = init_test_db("token-refresh-next-timer").await;
    let store = PgAccountStore::new(pool);
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
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

    let stored = wait_for_account(&store, "acct-refresh-next-timer", |account| {
        account.access_token.expose_secret() == new_access_token.as_str()
    })
    .await;

    assert_eq!(summary.scheduled, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    assert_eq!(task.scheduled_timer_count().await, 1);
    task.shutdown().await;
}
