use std::env;

use chrono::{DateTime, Utc};
use codex_proxy_rs::{
    accounts::{cookies::PgCookieStore, refresh::RedisRefreshLeaseStore, store::PgAccountStore},
    auth::store::{PgAdminUserStore, RedisAdminSessionStore},
    bootstrap::services::BackgroundTaskStores,
    dispatch::affinity::RedisSessionAffinityStore,
    infra::{database::migrate, redis::RedisConnection},
    keys::store::PgClientKeyStore,
    models::store::RedisModelSnapshotStore,
    telemetry::{
        account_usage::store::PgAccountUsageStore, buckets::store::PgRequestBucketStore,
        ops::store::PgOpsErrorLogStore, usage::store::PgUsageRecordStore,
    },
    upstream::openai::fingerprint::PgFingerprintStore,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

const DEFAULT_TEST_DATABASE_URL: &str =
    "postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy";
const DEFAULT_TEST_REDIS_URL: &str = "redis://127.0.0.1:6379";
static DATABASE_INIT_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub(crate) async fn init_test_db(label: &str) -> (PgPool, tempfile::TempDir) {
    let guard = tempfile::tempdir().expect("test database guard");
    (create_test_database(label).await, guard)
}

pub(crate) async fn create_test_database(label: &str) -> PgPool {
    let _permit = DATABASE_INIT_LIMIT.acquire().await.unwrap();
    let database_name = test_database_name(label);
    let base_url = test_database_url();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&base_url)
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
        .connect(&database_url_for(&base_url, &database_name))
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

pub(crate) async fn create_test_redis(label: &str) -> RedisConnection {
    let label = sanitize_label(label);
    let prefix = format!("cpr:test:{label}:{}", Uuid::new_v4().simple());
    RedisConnection::connect(&test_redis_url(), prefix)
        .await
        .unwrap_or_else(|error| panic!("connect CPR_TEST_REDIS_URL: {error}"))
}

pub(crate) fn background_task_stores(pool: PgPool, redis: RedisConnection) -> BackgroundTaskStores {
    BackgroundTaskStores {
        redis: redis.clone(),
        accounts: PgAccountStore::new(pool.clone()),
        admin_users: PgAdminUserStore::new(pool.clone()),
        admin_sessions: RedisAdminSessionStore::new(redis.clone()),
        cookies: PgCookieStore::new(pool.clone()),
        fingerprints: PgFingerprintStore::new(pool.clone()),
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
    env::var("CPR_TEST_DATABASE_URL").unwrap_or_else(|_| DEFAULT_TEST_DATABASE_URL.to_string())
}

pub(crate) fn test_redis_url() -> String {
    env::var("CPR_TEST_REDIS_URL").unwrap_or_else(|_| DEFAULT_TEST_REDIS_URL.to_string())
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
