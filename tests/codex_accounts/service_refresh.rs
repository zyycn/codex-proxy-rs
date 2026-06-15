use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use secrecy::SecretString;
use tokio::sync::Mutex;

use codex_proxy_rs::{
    codex::{
        accounts::{
            model::AccountStatus,
            pool::AccountPool,
            repository::{AccountRepository, AccountUsageRepository, NewAccount, TokenUpdate},
            service::{AccountService, AccountServiceDependencies},
        },
        gateway::{
            fingerprint::model::Fingerprint,
            oauth::{RefreshFailure, TokenPair, TokenRefresher},
            transport::websocket::CodexWebSocketPool,
        },
    },
    platform::{crypto::SecretBox, storage::db::connect_sqlite},
};

use crate::support::admin_accounts::test_config;

#[tokio::test]
async fn account_service_refresh_should_retry_with_latest_disk_refresh_token_after_invalid_grant() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool.clone(), SecretBox::new([19u8; 32]));
    repo.insert(NewAccount {
        id: "acct_stale".to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("access-old".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-old".to_string().into())),
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let refresher = DiskRotatingRefresher {
        repo: repo.clone(),
        calls: calls.clone(),
        first_call_seen: Arc::new(AtomicUsize::new(0)),
    };
    let service = AccountService::new(
        Arc::new(test_config(url)),
        AccountServiceDependencies {
            repository: Some(repo),
            usage_repository: Some(AccountUsageRepository::new(pool)),
            cookie_repository: None,
            token_refresher: Some(Arc::new(refresher)),
            account_pool: Arc::new(Mutex::new(AccountPool::default())),
            websocket_pool: Arc::new(CodexWebSocketPool::with_default_max_age()),
            fingerprint: Fingerprint::default_for_tests(),
        },
    );

    let result = service.refresh_account("acct_stale").await.unwrap();

    assert_eq!(result.outcome.as_str(), "alive");
    assert_eq!(
        calls.lock().await.as_slice(),
        ["refresh-old", "refresh-latest"]
    );
}

#[tokio::test]
async fn account_service_refresh_probe_should_not_persist_permanent_status_on_first_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts_scheduler.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool.clone(), SecretBox::new([20u8; 32]));
    repo.insert(NewAccount {
        id: "acct_scheduler".to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("access-old".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-old".to_string().into())),
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    let service = AccountService::new(
        Arc::new(test_config(url)),
        AccountServiceDependencies {
            repository: Some(repo.clone()),
            usage_repository: Some(AccountUsageRepository::new(pool)),
            cookie_repository: None,
            token_refresher: Some(Arc::new(AlwaysFailingRefresher)),
            account_pool: Arc::new(Mutex::new(AccountPool::default())),
            websocket_pool: Arc::new(CodexWebSocketPool::with_default_max_age()),
            fingerprint: Fingerprint::default_for_tests(),
        },
    );

    let result = service
        .probe_account_refresh("acct_scheduler")
        .await
        .unwrap();

    assert_eq!(result.outcome.as_str(), "dead");
    assert!(result.status.is_none());
    let stored = repo.get("acct_scheduler").await.unwrap().unwrap();
    assert_eq!(stored.status, AccountStatus::Active);
}

#[derive(Clone)]
struct DiskRotatingRefresher {
    repo: AccountRepository,
    calls: Arc<Mutex<Vec<String>>>,
    first_call_seen: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct AlwaysFailingRefresher;

#[async_trait]
impl TokenRefresher for AlwaysFailingRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(RefreshFailure::InvalidGrant)
    }
}

#[async_trait]
impl TokenRefresher for DiskRotatingRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        if refresh_token == "refresh-old" {
            self.first_call_seen.fetch_add(1, Ordering::SeqCst);
            self.repo
                .update_tokens(
                    "acct_stale",
                    TokenUpdate {
                        access_token: SecretString::new("external-access".to_string().into()),
                        refresh_token: Some(SecretString::new("refresh-latest".to_string().into())),
                        access_token_expires_at: None,
                    },
                )
                .await
                .unwrap();
            tokio::time::sleep(StdDuration::from_millis(5)).await;
            return Err(RefreshFailure::InvalidGrant);
        }
        if refresh_token == "refresh-latest" {
            return Ok(TokenPair {
                access_token: "access-after-retry".to_string(),
                refresh_token: Some("refresh-after-retry".to_string()),
            });
        }
        Err(RefreshFailure::InvalidGrant)
    }
}
