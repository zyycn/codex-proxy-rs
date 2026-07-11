use super::*;

#[tokio::test]
async fn token_refresh_task_should_keep_business_status_before_refresher_call() {
    let (pool, _guard) = init_test_db("token-refresh-status").await;
    let store = PgAccountStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 9, 0, 0).unwrap();
    let new_expires_at = now + Duration::hours(1);
    let new_access_token = test_jwt(new_expires_at.timestamp());
    store
        .insert(NewAccount {
            id: "acct-status-marker".to_string(),
            email: Some("marker@example.com".to_string()),
            account_id: Some("chatgpt-marker".to_string()),
            user_id: Some("user-marker".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new("refresh-marker".to_string().into())),
            added_at: None,
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let observed_statuses = Arc::new(Mutex::new(Vec::new()));
    let refresher = StoreObservingTokenRefresher {
        store: store.clone(),
        account_id: "acct-status-marker".to_string(),
        observed_statuses: observed_statuses.clone(),
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
        refresher,
    );

    let timer_summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("refresh timer should be scheduled");
    let stored = wait_for_account(&store, "acct-status-marker", |account| {
        account.access_token.expose_secret() == new_access_token.as_str()
    })
    .await;
    task.shutdown().await;
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(timer_summary.immediate, 1);
    assert_eq!(observed_statuses, [AccountStatus::Active]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_skip_expired_account_after_restart() {
    let (pool, _guard) = init_test_db("token-refresh-recovery").await;
    let store = PgAccountStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 10, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-expired-recovery".to_string(),
            email: Some("recovery@example.com".to_string()),
            account_id: Some("chatgpt-recovery".to_string()),
            user_id: Some("user-recovery".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.into()),
            refresh_token: Some(SecretString::new("refresh-recovery".to_string().into())),
            added_at: None,
            access_token_expires_at: Some(now + Duration::hours(1)),
            status: AccountStatus::Expired,
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

    let timer_summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("expired account should be skipped");
    let stored = store
        .get("acct-expired-recovery")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(timer_summary.skipped, 1);
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(stored.status, AccountStatus::Expired);
    assert!(stored.next_refresh_at.is_none());
}

#[tokio::test]
async fn token_refresh_task_should_retry_transport_failure_before_recovery() {
    let (pool, _guard) = init_test_db("token-refresh-transport-failure").await;
    let store = PgAccountStore::new(pool);
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
            added_at: None,
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let timer_summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("refresh timer should be scheduled");
    let stored = wait_for_account(&store, "acct-refresh-transport", |account| {
        account.next_refresh_at.is_some_and(|next| next > now)
    })
    .await;
    task.shutdown().await;
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(timer_summary.immediate, 1);
    assert_eq!(observed_statuses, [AccountStatus::Active; 5]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert!(stored.next_refresh_at.is_some_and(|next| next > now));
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_delay_recovery_after_retry_exhaustion() {
    let (pool, _guard) = init_test_db("token-refresh-delayed-recovery").await;
    let store = PgAccountStore::new(pool);
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
            added_at: None,
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let mut responses = Vec::new();
    responses.extend((0..5).map(|_| Err(RefreshFailure::RetryableTransport)));
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let failed = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("first refresh timer should be scheduled");
    let recovery_at = wait_for_account(&store, "acct-refresh-delayed-recovery", |account| {
        account.next_refresh_at.is_some_and(|next| next > now)
    })
    .await
    .next_refresh_at
    .expect("retry exhaustion should persist recovery time");
    let delayed = task
        .schedule_account_timers_once_at(now + Duration::minutes(5))
        .await
        .expect("recovery timer should be scheduled");
    let refreshed = task
        .schedule_account_timers_once_at(recovery_at + Duration::seconds(1))
        .await
        .expect("due recovery timer should replace future timer");
    let stored = wait_for_account(&store, "acct-refresh-delayed-recovery", |account| {
        account.access_token.expose_secret() == new_access_token.as_str()
    })
    .await;
    task.shutdown().await;
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(failed.immediate, 1);
    assert!(recovery_at >= now + Duration::minutes(8));
    assert!(recovery_at <= now + Duration::minutes(12));
    assert_eq!(delayed.scheduled, 1);
    assert_eq!(refreshed.immediate, 1);
    assert_eq!(refreshed.replaced, 1);
    assert_eq!(observed_statuses, [AccountStatus::Active; 6]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}

#[tokio::test]
async fn token_refresh_task_should_not_reuse_stale_refresh_token_after_retryable_failure() {
    let (pool, _guard) = init_test_db("token-refresh-stale-rt").await;
    let store = PgAccountStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 16, 0, 0).unwrap();
    let old_access_token = test_jwt((now + Duration::seconds(30)).timestamp());
    let rotated_access_token = test_jwt((now + Duration::hours(1)).timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-stale-rt".to_string(),
            email: Some("stale-rt@example.com".to_string()),
            account_id: Some("chatgpt-stale-rt".to_string()),
            user_id: Some("user-stale-rt".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(old_access_token.into()),
            refresh_token: Some(SecretString::new("refresh-stale-old".to_string().into())),
            added_at: None,
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let calls = Arc::new(AtomicUsize::new(0));
    let refresher = RefreshTokenRotatingRefresher {
        store: store.clone(),
        account_id: "acct-refresh-stale-rt".to_string(),
        calls: calls.clone(),
        access_token: rotated_access_token.clone(),
    };
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let timer_summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("refresh timer should be scheduled");
    let stored = wait_for_account(&store, "acct-refresh-stale-rt", |account| {
        account
            .refresh_token
            .as_ref()
            .is_some_and(|token| token.expose_secret() == "refresh-rotated")
    })
    .await;
    task.shutdown().await;

    assert_eq!(timer_summary.immediate, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        stored.access_token.expose_secret(),
        rotated_access_token.as_str()
    );
    assert_eq!(
        stored
            .refresh_token
            .as_ref()
            .map(secrecy::ExposeSecret::expose_secret),
        Some("refresh-rotated")
    );
}

#[tokio::test]
async fn token_refresh_task_should_confirm_invalid_grant_before_expiring_account() {
    let (pool, _guard) = init_test_db("token-refresh-permanent-failure").await;
    let store = PgAccountStore::new(pool);
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
            added_at: None,
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
    let task = codex_proxy_rs::fleet::refresh::TokenRefreshService::new(
        store.clone(),
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 1,
        },
        refresher,
    )
    .with_retry_delays(vec![StdDuration::ZERO; 4]);

    let timer_summary = task
        .schedule_account_timers_once_at(now)
        .await
        .expect("refresh timer should be scheduled");
    let stored = wait_for_account(&store, "acct-refresh-invalid-grant", |account| {
        account.status == AccountStatus::Expired
    })
    .await;
    task.shutdown().await;
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(timer_summary.immediate, 1);
    assert_eq!(observed_statuses, [AccountStatus::Active; 2]);
    assert_eq!(stored.status, AccountStatus::Expired);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
}
