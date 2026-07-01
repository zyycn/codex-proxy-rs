use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use codex_proxy_rs::admin::auth::service::SqliteAdminSessionStore;
use codex_proxy_rs::infra::database::connect_sqlite;
use codex_proxy_rs::proxy::dispatch::session_affinity::SessionAffinityEntry;
use codex_proxy_rs::proxy::dispatch::session_affinity::SqliteSessionAffinityStore;
use codex_proxy_rs::upstream::accounts::cookies::SqliteCookieStore;
use codex_proxy_rs::upstream::accounts::model::{Account, AccountStatus};
use codex_proxy_rs::upstream::accounts::model::{AccountModelUsageDelta, AccountUsageDelta};
use codex_proxy_rs::upstream::accounts::store::{AccountStore, AccountStoreResult};
use codex_proxy_rs::upstream::fingerprint::FingerprintRepository;
use codex_proxy_rs::upstream::models::ModelConfig;
use codex_proxy_rs::upstream::models::ModelService;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

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

    async fn update_quota_json(
        &self,
        _account_id: &str,
        _quota_json: &str,
    ) -> AccountStoreResult<bool> {
        Ok(false)
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
    repo: &FingerprintRepository,
) -> codex_proxy_rs::upstream::fingerprint::Fingerprint {
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

async fn insert_account(pool: &sqlx::SqlitePool, account_id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at) values (?, ?, 'active', ?, ?)",
    )
    .bind(account_id)
    .bind("access-token")
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .expect("account should be inserted");
}

async fn insert_cookie(
    pool: &sqlx::SqlitePool,
    id: &str,
    account_id: &str,
    name: &str,
    expires_at: chrono::DateTime<Utc>,
) {
    sqlx::query(
        "insert into account_cookies (id, account_id, domain, name, value, path, expires_at, updated_at) values (?, ?, 'chatgpt.com', ?, 'cipher', '/', ?, ?)",
    )
    .bind(id)
    .bind(account_id)
    .bind(name)
    .bind(expires_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .expect("cookie should be inserted");
}

async fn insert_admin_user(pool: &sqlx::SqlitePool, user_id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, 'hash', ?, ?)",
    )
    .bind(user_id)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .expect("admin user should be inserted");
}

async fn insert_admin_session(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    user_id: &str,
    expires_at: chrono::DateTime<Utc>,
) {
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(expires_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .expect("admin session should be inserted");
}

async fn cookie_ids(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar::<_, String>("select id from account_cookies order by id")
        .fetch_all(pool)
        .await
        .expect("cookie ids should load")
}

async fn admin_session_ids(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar::<_, String>("select id from admin_sessions order by id")
        .fetch_all(pool)
        .await
        .expect("session ids should load")
}

async fn session_affinity_response_ids(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "select response_id from session_affinities order by response_id",
    )
    .fetch_all(pool)
    .await
    .expect("session affinity response ids should load")
}

fn session_affinity_entry(
    account_id: &str,
    created_at: chrono::DateTime<Utc>,
) -> SessionAffinityEntry {
    SessionAffinityEntry {
        account_id: account_id.to_string(),
        conversation_id: format!("conversation-{account_id}"),
        turn_state: Some(format!("turn-{account_id}")),
        instructions_hash: None,
        input_tokens: Some(1),
        function_call_ids: Vec::new(),
        variant_hash: None,
        created_at,
    }
}
