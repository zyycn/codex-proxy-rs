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

#[path = "token_refresh/token_refresh_failures.rs"]
mod token_refresh_failures;
#[path = "token_refresh/token_refresh_scheduling.rs"]
mod token_refresh_scheduling;
#[path = "token_refresh/token_refresh_success.rs"]
mod token_refresh_success;

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
