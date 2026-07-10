use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::accounts::account::AccountStatus;
use codex_proxy_rs::accounts::refresh::{RedisRefreshLeaseStore, RefreshPolicy};
use codex_proxy_rs::accounts::store::PgAccountStore;
use codex_proxy_rs::accounts::store::{AccountClaimsUpdate, NewAccount};
use codex_proxy_rs::upstream::openai::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use tokio::{
    sync::{Mutex, Notify},
    time::{sleep, timeout},
};

use crate::support::storage::{create_test_redis, init_test_db};

mod failures;
mod scheduling;
mod success;

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
    store: PgAccountStore,
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
    store: PgAccountStore,
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

#[derive(Clone)]
struct RefreshTokenRotatingRefresher {
    store: PgAccountStore,
    account_id: String,
    calls: Arc<AtomicUsize>,
    access_token: String,
}

#[async_trait]
impl TokenRefresher for RefreshTokenRotatingRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.store
            .update_from_claims(
                &self.account_id,
                AccountClaimsUpdate {
                    email: Some("rotated@example.com".to_string()),
                    account_id: Some("chatgpt-rotated".to_string()),
                    user_id: Some("user-rotated".to_string()),
                    plan_type: Some("plus".to_string()),
                    access_token: SecretString::new(self.access_token.clone().into()),
                    refresh_token: Some(SecretString::new("refresh-rotated".to_string().into())),
                    access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
                    next_refresh_at: None,
                    status: AccountStatus::Active,
                },
            )
            .await
            .expect("rotated token should persist");
        Err(RefreshFailure::RetryableTransport)
    }
}

fn test_jwt(exp: i64) -> String {
    let payload = json!({ "exp": exp });
    crate::support::jwt::unsigned_jwt(&payload)
}
