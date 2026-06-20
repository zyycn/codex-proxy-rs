use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use codex_proxy_adapters::{
    codex::fingerprint::FingerprintRepository,
    sqlite::{
        admin_sessions::SqliteAdminSessionStore, cookies::SqliteCookieStore,
        session_affinity::SqliteSessionAffinityStore,
    },
};
use codex_proxy_core::{
    accounts::{
        model::{Account, AccountStatus},
        ports::{AccountStore, AccountStoreResult},
        usage::AccountUsageDelta,
    },
    models::{model::ModelConfig, service::ModelService},
    serving::affinity::SessionAffinityEntry,
};
use codex_proxy_platform::config::{
    AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig,
    ModelConfig as PlatformModelConfig, QuotaConfig, QuotaWarningThresholds, SecurityConfig,
    ServerConfig, TlsConfig, UsageStatsConfig,
};
use codex_proxy_platform::{crypto::SecretBox, storage::connect_sqlite};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[path = "tasks/cleanup.rs"]
mod cleanup;
#[path = "tasks/coordinator.rs"]
mod coordinator;
#[path = "tasks/fingerprint.rs"]
mod fingerprint;
#[path = "tasks/model_refresh.rs"]
mod model_refresh;

struct FakeAccountStore;

#[async_trait]
impl AccountStore for FakeAccountStore {
    async fn list_pool_accounts(&self) -> AccountStoreResult<Vec<Account>> {
        Ok(vec![Account::test("acct-1", AccountStatus::Active)])
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
) -> codex_proxy_core::gateway::fingerprint::Fingerprint {
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
        "insert into accounts (id, access_token_cipher, status, added_at, updated_at) values (?, ?, 'active', ?, ?)",
    )
    .bind(account_id)
    .bind("cipher")
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
        "insert into account_cookies (id, account_id, domain, name, value_cipher, path, expires_at, updated_at) values (?, ?, 'chatgpt.com', ?, 'cipher', '/', ?, ?)",
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

fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://example.invalid".to_string(),
        },
        model: PlatformModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: Default::default(),
        },
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
            oauth_client_id: "app_id".to_string(),
            oauth_auth_endpoint: "https://auth.invalid".to_string(),
            oauth_token_endpoint: "https://token.invalid".to_string(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            warning_thresholds: QuotaWarningThresholds {
                primary: vec![80, 90],
                secondary: vec![80, 90],
            },
            skip_exhausted: true,
        },
        usage_stats: UsageStatsConfig {
            history_retention_days: None,
        },
        database: DatabaseConfig { url: database_url },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
        ws_pool: Default::default(),
        fingerprint: Default::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            session_cleanup_interval_secs: 3600,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}
