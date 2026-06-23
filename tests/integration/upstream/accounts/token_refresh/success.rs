use super::*;

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
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
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
    let task = codex_proxy_rs::upstream::accounts::token_refresh::RuntimeTokenRefreshService::new(
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
