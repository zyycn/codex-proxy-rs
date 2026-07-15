use std::{env, sync::OnceLock, thread};

use chrono::{DateTime, Utc};
use codex_proxy_rs::{
    auth::store::{PgAdminUserStore, RedisAdminSessionStore},
    bootstrap::{config::BootstrapConfig, services::BackgroundTaskStores},
    dispatch::affinity::RedisSessionAffinityStore,
    fleet::{cookies::PgCookieStore, refresh::RedisRefreshLeaseStore, store::PgAccountStore},
    infra::{database::migrate, redis::RedisConnection},
    keys::store::PgClientKeyStore,
    models::store::RedisModelSnapshotStore,
    telemetry::{
        account_usage::store::PgAccountUsageStore, buckets::store::PgRequestBucketStore,
        ops::store::PgOpsErrorLogStore, usage::store::PgUsageRecordStore,
    },
};
use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

const DEFAULT_TEST_DATABASE_URL: &str =
    "postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy";
const DEFAULT_TEST_REDIS_URL: &str = "redis://127.0.0.1:6379";
static DATABASE_INIT_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static REDIS_TEST_CLEANUP: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static REDIS_TEST_RUN_ID: OnceLock<String> = OnceLock::new();
static LOCAL_BOOTSTRAP_CONFIG: OnceLock<Option<BootstrapConfig>> = OnceLock::new();

pub(crate) struct TestDatabaseGuard {
    database_name: Option<String>,
    base_url: String,
    workspace: tempfile::TempDir,
}

impl TestDatabaseGuard {
    pub(crate) fn path(&self) -> &std::path::Path {
        self.workspace.path()
    }
}

impl Drop for TestDatabaseGuard {
    fn drop(&mut self) {
        let Some(database_name) = self.database_name.take() else {
            return;
        };
        let base_url = self.base_url.clone();
        let thread_name = format!("drop-{database_name}");
        let cleanup = thread::Builder::new().name(thread_name).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                eprintln!("failed to create runtime for test database cleanup: {database_name}");
                return;
            };
            if let Err(error) = runtime.block_on(drop_test_database(&base_url, &database_name)) {
                eprintln!("failed to drop test database {database_name}: {error}");
            }
        });
        match cleanup {
            Ok(cleanup) => {
                if cleanup.join().is_err() {
                    eprintln!("test database cleanup thread panicked");
                }
            }
            Err(error) => eprintln!("failed to start test database cleanup thread: {error}"),
        }
    }
}

pub(crate) async fn init_test_db(label: &str) -> (PgPool, TestDatabaseGuard) {
    let database_name = test_database_name(label);
    let base_url = test_database_url();
    let guard = TestDatabaseGuard {
        database_name: Some(database_name.clone()),
        base_url: base_url.clone(),
        workspace: tempfile::tempdir().expect("test database workspace"),
    };
    let pool = create_test_database(&base_url, &database_name).await;
    (pool, guard)
}

async fn create_test_database(base_url: &str, database_name: &str) -> PgPool {
    let _permit = DATABASE_INIT_LIMIT.acquire().await.unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(base_url)
        .await
        .unwrap_or_else(|error| panic!("connect CPR_TEST_DATABASE_URL: {error}"));
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        r#"create database "{database_name}""#
    )))
    .execute(&admin)
    .await
    .unwrap_or_else(|error| panic!("create test database {database_name}: {error}"));
    admin.close().await;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url_for(base_url, database_name))
        .await
        .unwrap_or_else(|error| panic!("connect test database {database_name}: {error}"));
    sqlx::query("set max_parallel_workers_per_gather = 0")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("set max_parallel_maintenance_workers = 0")
        .execute(&pool)
        .await
        .unwrap();
    migrate(&pool)
        .await
        .unwrap_or_else(|error| panic!("initialize test database {database_name}: {error}"));
    pool
}

async fn drop_test_database(base_url: &str, database_name: &str) -> Result<(), sqlx::Error> {
    let mut connection = PgConnection::connect(base_url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        r#"drop database if exists "{database_name}" with (force)"#
    )))
    .execute(&mut connection)
    .await?;
    Ok(())
}

pub(crate) async fn create_test_redis(label: &str) -> RedisConnection {
    let redis_url = test_redis_url();
    REDIS_TEST_CLEANUP
        .get_or_init(|| async {
            cleanup_stale_test_redis_keys(&redis_url)
                .await
                .unwrap_or_else(|error| panic!("clean stale Redis test keys: {error}"));
        })
        .await;
    let label = sanitize_label(label);
    let run_id = REDIS_TEST_RUN_ID.get_or_init(|| Uuid::new_v4().simple().to_string());
    let prefix = format!("cpr:test:{run_id}:{label}:{}", Uuid::new_v4().simple());
    RedisConnection::connect(&redis_url, prefix)
        .await
        .unwrap_or_else(|error| panic!("connect CPR_TEST_REDIS_URL: {error}"))
}

async fn cleanup_stale_test_redis_keys(redis_url: &str) -> redis::RedisResult<()> {
    let client = redis::Client::open(redis_url)?;
    let mut connection = client.get_connection_manager().await?;
    let mut cursor = 0u64;
    loop {
        let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg("cpr:test:*")
            .arg("COUNT")
            .arg(500)
            .query_async(&mut connection)
            .await?;
        if !keys.is_empty() {
            let _: usize = redis::cmd("UNLINK")
                .arg(keys)
                .query_async(&mut connection)
                .await?;
        }
        if next_cursor == 0 {
            return Ok(());
        }
        cursor = next_cursor;
    }
}

pub(crate) fn background_task_stores(pool: PgPool, redis: RedisConnection) -> BackgroundTaskStores {
    BackgroundTaskStores {
        redis: redis.clone(),
        accounts: PgAccountStore::new(pool.clone()),
        admin_users: PgAdminUserStore::new(pool.clone()),
        admin_sessions: RedisAdminSessionStore::new(redis.clone()),
        cookies: PgCookieStore::new(pool.clone()),
        session_affinity: RedisSessionAffinityStore::new(redis.clone()),
        refresh_leases: RedisRefreshLeaseStore::new(redis.clone()),
        model_snapshots: RedisModelSnapshotStore::new(redis),
        client_keys: PgClientKeyStore::new(pool.clone()),
        usage_records: PgUsageRecordStore::new(pool.clone()),
        ops_errors: PgOpsErrorLogStore::new(pool.clone()),
        account_usage: PgAccountUsageStore::new(pool.clone()),
        request_buckets: PgRequestBucketStore::new(pool),
    }
}

pub(crate) fn test_database_url() -> String {
    // 显式测试拓扑优先；本地开发默认跟随 Compose 配置；无本地配置的 CI 使用固定测试值。
    env::var("CPR_TEST_DATABASE_URL")
        .ok()
        .or_else(|| local_bootstrap_config().map(|config| config.database_url().to_string()))
        .unwrap_or_else(|| DEFAULT_TEST_DATABASE_URL.to_string())
}

pub(crate) fn test_redis_url() -> String {
    env::var("CPR_TEST_REDIS_URL")
        .ok()
        .or_else(|| local_bootstrap_config().map(|config| config.redis_url().to_string()))
        .unwrap_or_else(|| DEFAULT_TEST_REDIS_URL.to_string())
}

fn local_bootstrap_config() -> Option<&'static BootstrapConfig> {
    LOCAL_BOOTSTRAP_CONFIG
        .get_or_init(|| {
            let path =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/config.yaml");
            if !path.is_file() {
                return None;
            }
            Some(
                BootstrapConfig::load_from_path(path)
                    .unwrap_or_else(|error| panic!("load local test service config: {error}")),
            )
        })
        .as_ref()
}

pub(crate) fn timestamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap_or_else(|error| panic!("invalid test timestamp {value}: {error}"))
        .to_utc()
}

fn test_database_name(label: &str) -> String {
    let label = sanitize_label(label);
    let suffix = Uuid::new_v4().simple().to_string();
    format!(
        "cpr_test_{}_{}",
        label.chars().take(24).collect::<String>(),
        &suffix[..12]
    )
}

fn sanitize_label(label: &str) -> String {
    let label = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let label = label.trim_matches('_');
    if label.is_empty() {
        "case".to_string()
    } else {
        label.to_string()
    }
}

fn database_url_for(base_url: &str, database_name: &str) -> String {
    let (without_query, query) = base_url
        .split_once('?')
        .map_or((base_url, None), |(url, query)| (url, Some(query)));
    let slash = without_query
        .rfind('/')
        .unwrap_or_else(|| panic!("CPR_TEST_DATABASE_URL must include a database name"));
    let mut url = format!("{}/{database_name}", &without_query[..slash]);
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    url
}
