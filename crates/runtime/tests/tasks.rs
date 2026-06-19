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

#[tokio::test]
async fn fingerprint_update_task_should_start_background_checker() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);

    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        Some(repo),
        "https://example.invalid/appcast.xml".to_string(),
        dir.path().join("extracted-fingerprint.json"),
        "26.800.1".to_string(),
        "6001".to_string(),
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn fingerprint_update_task_should_apply_initial_appcast_update_to_repository() {
    let server = MockServer::start().await;
    mount_appcast(
        &server,
        r#"
        <rss>
          <channel>
            <item>
              <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
            </item>
          </channel>
        </rss>
        "#,
    )
    .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints-initial-update.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);
    let extracted_path = dir.path().join("extracted-fingerprint.json");
    std::fs::write(
        &extracted_path,
        r#"{"app_version":"26.900.1","build_number":"7001","chromium_version":"147"}"#,
    )
    .expect("extracted fingerprint should be written");

    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        Some(repo.clone()),
        format!("{}/appcast.xml", server.uri()),
        extracted_path,
        "26.800.1".to_string(),
        "6001".to_string(),
    );

    let stored = wait_for_auto_updated_fingerprint(&repo).await;
    handle.shutdown().await;

    assert_eq!(
        (
            stored.app_version.as_str(),
            stored.build_number.as_str(),
            stored.chromium_version.as_str()
        ),
        ("26.900.1", "7001", "147")
    );
}

#[tokio::test]
async fn fingerprint_update_task_should_not_persist_when_appcast_matches_current_fingerprint() {
    let server = MockServer::start().await;
    mount_appcast(
        &server,
        r#"
        <rss>
          <channel>
            <item>
              <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
            </item>
          </channel>
        </rss>
        "#,
    )
    .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints-no-update.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);
    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        Some(repo.clone()),
        format!("{}/appcast.xml", server.uri()),
        dir.path().join("missing-extracted-fingerprint.json"),
        "26.900.1".to_string(),
        "7001".to_string(),
    );

    wait_for_appcast_requests(&server, 1).await;
    handle.shutdown().await;
    let stored = repo
        .load_latest_auto_updated()
        .await
        .expect("latest auto update should load");

    assert!(stored.is_none());
}

#[tokio::test]
async fn fingerprint_update_task_should_check_appcast_without_repository() {
    let server = MockServer::start().await;
    mount_appcast(
        &server,
        r#"
        <rss>
          <channel>
            <item>
              <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
            </item>
          </channel>
        </rss>
        "#,
    )
    .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        None,
        format!("{}/appcast.xml", server.uri()),
        dir.path().join("missing-extracted-fingerprint.json"),
        "26.800.1".to_string(),
        "6001".to_string(),
    );

    wait_for_appcast_requests(&server, 1).await;
    handle.shutdown().await;
}

#[tokio::test]
async fn model_refresh_task_should_start_and_shutdown() {
    let model_service = Arc::new(ModelService::new(
        ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: Default::default(),
        },
        None,
        None,
        None,
    ));
    let account_store = Arc::new(FakeAccountStore);

    let handle = codex_proxy_runtime::tasks::model_refresh::ModelRefreshTask::new(
        model_service,
        account_store,
    )
    .start();

    handle.shutdown().await;
}

#[tokio::test]
async fn cookie_cleanup_task_should_delete_only_expired_cookie_rows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("cookies.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let secret_box = SecretBox::new([7u8; 32]);
    let store = SqliteCookieStore::new(pool.clone(), secret_box);
    insert_account(&pool, "acct-cookie").await;
    let now = Utc::now();
    insert_cookie(
        &pool,
        "expired-cookie",
        "acct-cookie",
        "old",
        now - Duration::minutes(1),
    )
    .await;
    insert_cookie(
        &pool,
        "active-cookie",
        "acct-cookie",
        "fresh",
        now + Duration::minutes(10),
    )
    .await;

    let deleted = codex_proxy_runtime::tasks::cookie_cleanup::CookieCleanupTask::new(store)
        .cleanup_once_at(now)
        .await
        .expect("cookie cleanup should succeed");

    let remaining = cookie_ids(&pool).await;

    assert_eq!((deleted, remaining), (1, vec!["active-cookie".to_string()]));
}

#[tokio::test]
async fn session_cleanup_task_should_delete_only_expired_sessions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("sessions.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAdminSessionStore::new(pool.clone());
    let now = Utc::now();
    insert_admin_user(&pool, "admin").await;
    insert_admin_session(
        &pool,
        "expired-session",
        "admin",
        now - Duration::minutes(1),
    )
    .await;
    insert_admin_session(
        &pool,
        "active-session",
        "admin",
        now + Duration::minutes(10),
    )
    .await;

    let deleted = codex_proxy_runtime::tasks::session_cleanup::SessionCleanupTask::new(store, 3600)
        .cleanup_once_at(now)
        .await
        .expect("session cleanup should succeed");

    let remaining = admin_session_ids(&pool).await;

    assert_eq!(
        (deleted, remaining),
        (1, vec!["active-session".to_string()])
    );
}

#[tokio::test]
async fn session_affinity_cleanup_task_should_delete_only_expired_affinities() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("session-affinity-cleanup.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteSessionAffinityStore::new(pool.clone());
    let now = Utc::now();
    store
        .upsert(
            "expired-response",
            &session_affinity_entry("expired-account", now - Duration::hours(2)),
            Duration::hours(1),
        )
        .await
        .expect("expired affinity should be inserted");
    store
        .upsert(
            "active-response",
            &session_affinity_entry("active-account", now),
            Duration::hours(1),
        )
        .await
        .expect("active affinity should be inserted");

    let deleted =
        codex_proxy_runtime::tasks::session_affinity_cleanup::SessionAffinityCleanupTask::new(
            store, 3600,
        )
        .cleanup_once_at(now)
        .await
        .expect("session affinity cleanup should succeed");

    let remaining = session_affinity_response_ids(&pool).await;

    assert_eq!(
        (deleted, remaining),
        (1, vec!["active-response".to_string()])
    );
}

#[tokio::test]
async fn start_background_tasks_should_register_migrated_runtime_tasks() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("background-tasks.sqlite");
    let database_url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&database_url).await.expect("sqlite pool");
    let state = codex_proxy_runtime::state::AppState::with_pool_and_secret_box(
        test_config(database_url),
        pool,
        SecretBox::new([9u8; 32]),
    );

    let coordinator = codex_proxy_runtime::tasks::coordinator::start_background_tasks(&state).await;
    let task_names = coordinator.task_names();

    assert_eq!(
        task_names,
        [
            "cookie_cleanup",
            "session_cleanup",
            "session_affinity_cleanup",
            "model_refresh",
            "token_refresh",
            "quota_refresh",
            "fingerprint_update"
        ]
    );

    coordinator.shutdown().await;
}

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

async fn wait_for_auto_updated_fingerprint(
    repo: &FingerprintRepository,
) -> codex_proxy_core::gateway::fingerprint::Fingerprint {
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(2);
    loop {
        if let Some(stored) = repo
            .load_latest_auto_updated()
            .await
            .expect("latest auto update should load")
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
