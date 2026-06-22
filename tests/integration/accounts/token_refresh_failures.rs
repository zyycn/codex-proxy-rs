use super::*;

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
            added_at: None,
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
    let task = codex_proxy_rs::app::tasks::token_refresh::TokenRefreshTask::new(
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
            added_at: None,
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
    let task = codex_proxy_rs::app::tasks::token_refresh::TokenRefreshTask::new(
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
async fn token_refresh_task_should_not_retry_ambiguous_transport_failure() {
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
    let task = codex_proxy_rs::app::tasks::token_refresh::TokenRefreshTask::new(
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
    assert_eq!(observed_statuses, [AccountStatus::Refreshing]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert!(stored.next_refresh_at.is_some_and(|next| next > now));
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
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
    let task = codex_proxy_rs::app::tasks::token_refresh::TokenRefreshTask::new(
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
async fn token_refresh_task_should_not_reuse_stale_refresh_token_after_retryable_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("token-refresh-stale-rt.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool, SecretBox::new([31u8; 32]));
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
    let task = codex_proxy_rs::app::tasks::token_refresh::TokenRefreshTask::new(
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
        .expect("stale refresh token should skip retry");
    let stored = store
        .get("acct-refresh-stale-rt")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(summary.skipped, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        stored.access_token.expose_secret(),
        rotated_access_token.as_str()
    );
    assert_eq!(
        stored
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret()),
        Some("refresh-rotated")
    );
}

#[tokio::test]
async fn token_refresh_task_should_confirm_invalid_grant_before_disabling_account() {
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
    let task = codex_proxy_rs::app::tasks::token_refresh::TokenRefreshTask::new(
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
    assert_eq!(stored.status, AccountStatus::Disabled);
    assert_eq!(
        stored.access_token.expose_secret(),
        old_access_token.as_str()
    );
}
