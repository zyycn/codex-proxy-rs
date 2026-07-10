use super::*;

#[tokio::test]
async fn token_refresh_task_should_persist_refreshed_access_token_and_keep_refresh_token() {
    let (pool, _guard) = init_test_db("token-refresh").await;
    let store = PgAccountStore::new(pool);
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
            added_at: None,
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
    let task = codex_proxy_rs::accounts::refresh::RuntimeTokenRefreshService::new(
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
async fn token_refresh_task_should_persist_rotated_refresh_token_when_returned() {
    let (pool, _guard) = init_test_db("token-refresh-rotated").await;
    let store = PgAccountStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 9, 0, 0).unwrap();
    let new_expires_at = now + Duration::hours(1);
    let new_access_token = test_jwt(new_expires_at.timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-rotated".to_string(),
            email: Some("rotated@example.com".to_string()),
            account_id: Some("chatgpt-rotated".to_string()),
            user_id: Some("user-rotated".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new("refresh-old".to_string().into())),
            added_at: None,
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    let refresher = StaticTokenRefresher {
        response: Arc::new(Mutex::new(Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: Some("refresh-new".to_string()),
        }))),
    };
    let task = codex_proxy_rs::accounts::refresh::RuntimeTokenRefreshService::new(
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
        .get("acct-refresh-rotated")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(summary.refreshed, 1);
    assert_eq!(stored.access_token.expose_secret(), new_access_token);
    assert_eq!(
        stored
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("refresh-new")
    );
}

#[tokio::test]
async fn token_refresh_task_should_refresh_quota_exhausted_account_without_clearing_status() {
    let (pool, _guard) = init_test_db("token-refresh-quota-exhausted").await;
    let store = PgAccountStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 11, 0, 0).unwrap();
    let new_expires_at = now + Duration::hours(1);
    let new_access_token = test_jwt(new_expires_at.timestamp());
    store
        .insert(NewAccount {
            id: "acct-refresh-quota-exhausted".to_string(),
            email: Some("quota-exhausted@example.com".to_string()),
            account_id: Some("chatgpt-quota-exhausted".to_string()),
            user_id: Some("user-quota-exhausted".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now + Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new(
                "refresh-quota-exhausted".to_string().into(),
            )),
            added_at: None,
            access_token_expires_at: Some(now + Duration::seconds(30)),
            status: AccountStatus::QuotaExhausted,
        })
        .await
        .expect("account should be inserted");
    let observed_statuses = Arc::new(Mutex::new(Vec::new()));
    let refresher = StoreObservingTokenRefresher {
        store: store.clone(),
        account_id: "acct-refresh-quota-exhausted".to_string(),
        observed_statuses: observed_statuses.clone(),
        response: Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }),
    };
    let task = codex_proxy_rs::accounts::refresh::RuntimeTokenRefreshService::new(
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
        .expect("quota-exhausted account should refresh token");
    let stored = store
        .get("acct-refresh-quota-exhausted")
        .await
        .expect("account should load")
        .expect("account should exist");
    let observed_statuses = observed_statuses.lock().await.clone();

    assert_eq!(summary.refreshed, 1);
    assert_eq!(observed_statuses, [AccountStatus::QuotaExhausted]);
    assert_eq!(stored.status, AccountStatus::QuotaExhausted);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    assert_eq!(stored.access_token_expires_at, Some(new_expires_at));
    assert!(stored.next_refresh_at.is_some());
}

#[tokio::test]
async fn token_refresh_task_should_not_clear_quota_limit_when_exhausted_token_refreshes() {
    let (pool, _guard) = init_test_db("token-refresh-exhausted-quota").await;
    let store = PgAccountStore::new(pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 11, 30, 0).unwrap();
    let cooldown_until = now + Duration::hours(1);
    let new_expires_at = now + Duration::hours(2);
    let new_access_token = test_jwt(new_expires_at.timestamp());
    store
        .insert(NewAccount {
            id: "acct-exhausted-quota".to_string(),
            email: Some("exhausted-quota@example.com".to_string()),
            account_id: Some("chatgpt-exhausted-quota".to_string()),
            user_id: Some("user-exhausted-quota".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(
                test_jwt((now - Duration::seconds(30)).timestamp()).into(),
            ),
            refresh_token: Some(SecretString::new(
                "refresh-exhausted-quota".to_string().into(),
            )),
            added_at: None,
            access_token_expires_at: Some(now - Duration::seconds(30)),
            status: AccountStatus::QuotaExhausted,
        })
        .await
        .expect("account should be inserted");
    sqlx::query(
        "update accounts set quota_limit_reached = true, quota_cooldown_until = $1 where id = $2",
    )
    .bind(cooldown_until)
    .bind("acct-exhausted-quota")
    .execute(&pool)
    .await
    .unwrap();
    let refresher = StaticTokenRefresher {
        response: Arc::new(Mutex::new(Ok(TokenPair {
            access_token: new_access_token.clone(),
            refresh_token: None,
        }))),
    };
    let task = codex_proxy_rs::accounts::refresh::RuntimeTokenRefreshService::new(
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
        .expect("quota-exhausted account should refresh token");
    let stored = store
        .get("acct-exhausted-quota")
        .await
        .expect("account should load")
        .expect("account should exist");
    let quota_state: (bool, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = $1",
    )
    .bind("acct-exhausted-quota")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(summary.refreshed, 1);
    assert_eq!(stored.status, AccountStatus::QuotaExhausted);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
    assert!(quota_state.0);
    assert_eq!(quota_state.1, Some(cooldown_until));
}

#[tokio::test]
async fn token_refresh_task_should_persist_success_after_transient_transport_retry() {
    let (pool, _guard) = init_test_db("token-refresh-transient-success").await;
    let store = PgAccountStore::new(pool);
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
            added_at: None,
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
            Err(RefreshFailure::RetryableTransport),
            Ok(TokenPair {
                access_token: new_access_token.clone(),
                refresh_token: None,
            }),
        ]))),
    };
    let task = codex_proxy_rs::accounts::refresh::RuntimeTokenRefreshService::new(
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
    assert_eq!(observed_statuses, [AccountStatus::Active; 2]);
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(
        stored.access_token.expose_secret(),
        new_access_token.as_str()
    );
}
