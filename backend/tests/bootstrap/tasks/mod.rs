use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use codex_proxy_rs::accounts::account::{Account, AccountStatus};
use codex_proxy_rs::accounts::account::{AccountModelUsageDelta, AccountUsageDelta};
use codex_proxy_rs::accounts::cookies::PgCookieStore;
use codex_proxy_rs::accounts::store::{AccountStore, AccountStoreResult};
use codex_proxy_rs::models::config::ModelConfig;
use codex_proxy_rs::models::service::ModelService;
use codex_proxy_rs::upstream::openai::fingerprint::{PgFingerprintStore, RuntimeFingerprint};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use crate::support::storage::init_test_db;

mod cleanup;
mod coordinator;
mod fingerprint;
mod model_refresh;

struct FakeAccountStore;

#[async_trait]
impl AccountStore for FakeAccountStore {
    async fn list_pool_accounts(&self) -> AccountStoreResult<Vec<Account>> {
        Ok(vec![crate::support::accounts::test_account(
            "acct-1",
            AccountStatus::Active,
        )])
    }

    async fn mark_quota_limited_until(
        &self,
        _account_id: &str,
        _cooldown_until: chrono::DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        Ok(true)
    }

    async fn set_cloudflare_cooldown_until(
        &self,
        _account_id: &str,
        _cooldown_until: chrono::DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        Ok(true)
    }

    async fn set_status(
        &self,
        _account_id: &str,
        _status: AccountStatus,
    ) -> AccountStoreResult<bool> {
        Ok(true)
    }

    async fn record_usage_delta(
        &self,
        _account_id: &str,
        _usage: AccountUsageDelta,
    ) -> AccountStoreResult<()> {
        Ok(())
    }

    async fn record_model_usage_delta(
        &self,
        _account_id: &str,
        _model: &str,
        _usage: AccountModelUsageDelta,
    ) -> AccountStoreResult<()> {
        Ok(())
    }

    async fn get_quota_json(&self, _account_id: &str) -> AccountStoreResult<Option<String>> {
        Ok(None)
    }

    async fn apply_quota_snapshot(
        &self,
        _account_id: &str,
        _quota_json: &str,
        _limit_reached: bool,
        _cooldown_until: Option<chrono::DateTime<Utc>>,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    async fn sync_runtime_account_state(
        &self,
        _account: &Account,
        _sync_usage_window: bool,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    async fn sync_rate_limit_window(
        &self,
        _account_id: &str,
        _reset_at: chrono::DateTime<Utc>,
        _limit_window_seconds: Option<u64>,
    ) -> AccountStoreResult<()> {
        Ok(())
    }
}

async fn mount_appcast(server: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/appcast.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/xml"))
        .mount(server)
        .await;
}

async fn wait_for_current_fingerprint_version(
    repo: &PgFingerprintStore,
) -> codex_proxy_rs::upstream::openai::fingerprint::Fingerprint {
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(2);
    loop {
        if let Some(stored) = repo
            .load_current()
            .await
            .expect("current fingerprint should load")
            .filter(|stored| stored.app_version == "26.900.1")
        {
            return stored;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "fingerprint update task did not persist auto update before timeout"
        );
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }
}

async fn runtime_fingerprint_from_repo(repo: &PgFingerprintStore) -> RuntimeFingerprint {
    RuntimeFingerprint::new(
        repo.load_current()
            .await
            .expect("current fingerprint should load")
            .expect("current fingerprint should exist"),
    )
}

async fn wait_for_appcast_requests(server: &MockServer, expected_count: usize) {
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(2);
    loop {
        let request_count = server
            .received_requests()
            .await
            .expect("received requests should load")
            .iter()
            .filter(|request| request.url.path() == "/appcast.xml")
            .count();
        if request_count >= expected_count {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "fingerprint update task did not request appcast before timeout"
        );
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }
}

async fn insert_account(pool: &sqlx::PgPool, account_id: &str) {
    let now = Utc::now();
    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at) values ($1, $2, 'active', $3, $3)",
    )
    .bind(account_id)
    .bind("access-token")
    .bind(now)
    .execute(pool)
    .await
    .expect("account should be inserted");
}

async fn insert_cookie(
    pool: &sqlx::PgPool,
    id: &str,
    account_id: &str,
    name: &str,
    expires_at: chrono::DateTime<Utc>,
) {
    sqlx::query(
        "insert into account_cookies (id, account_id, domain, name, value, path, expires_at, updated_at) values ($1, $2, 'chatgpt.com', $3, 'cipher', '/', $4, $5)",
    )
    .bind(id)
    .bind(account_id)
    .bind(name)
    .bind(expires_at)
    .bind(Utc::now())
    .execute(pool)
    .await
    .expect("cookie should be inserted");
}

async fn cookie_ids(pool: &sqlx::PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, String>("select id from account_cookies order by id")
        .fetch_all(pool)
        .await
        .expect("cookie ids should load")
}
