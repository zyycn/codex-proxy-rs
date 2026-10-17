//! PostgreSQL 连接错误、迁移应用与事务归属测试

use std::error::Error as _;

use super::*;

#[tokio::test]
async fn cancelled_transaction_begin_should_return_a_clean_connection_to_the_pool() {
    let Some(database_url) = crate::support::test_env("CPR_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let observer = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let lock = i64::from_le_bytes(Uuid::new_v4().as_bytes()[..8].try_into().unwrap());
    sqlx::query("select pg_advisory_lock($1)")
        .bind(lock)
        .execute(&observer)
        .await
        .unwrap();
    let pending_pool = pool.clone();
    let begin = tokio::spawn(async move {
        pending_pool
            .begin_with(sqlx::AssertSqlSafe(format!(
                "begin; select pg_advisory_xact_lock({lock})"
            )))
            .await
    });
    // 等服务端进入 BEGIN 的同一次往返再取消，避免依赖客户端调度时机
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "select coalesce(wait_event = 'advisory', false) from pg_stat_activity where pid=$1",
            )
            .bind(pid)
            .fetch_one(&observer)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("BEGIN must reach PostgreSQL before cancellation");
    begin.abort();
    assert!(begin.await.unwrap_err().is_cancelled());
    sqlx::query("select pg_advisory_unlock($1)")
        .bind(lock)
        .execute(&observer)
        .await
        .unwrap();

    // 重新借出意味着池已处理被取消语句及待回滚帧；必须复用干净的原连接
    let mut connection = tokio::time::timeout(Duration::from_secs(5), pool.acquire())
        .await
        .unwrap()
        .unwrap();
    let reused_pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("select state from pg_stat_activity where pid=$1")
        .bind(pid)
        .fetch_one(&observer)
        .await
        .unwrap();
    drop(connection);
    pool.close().await;
    observer.close().await;

    assert_eq!(reused_pid, pid);
    assert_eq!(
        state, "idle",
        "cancelled BEGIN must not leak its transaction"
    );
}

#[tokio::test]
async fn invalid_postgres_options_preserve_the_sqlx_source() {
    let error = gateway_store::postgres::connect_and_migrate(
        "postgres://localhost:invalid-port/test",
        gateway_store::StorePoolConfig::default(),
    )
    .await
    .unwrap_err();

    let source = error.source().expect("PostgreSQL source");
    assert!(source.downcast_ref::<sqlx::Error>().is_some());
    assert!(source.source().is_some());
    assert!(
        error
            .to_string()
            .contains("parse PostgreSQL connection options")
    );
    assert!(!format!("{error:?} {error}").contains("invalid-port"));
}

#[tokio::test]
async fn connect_and_migrate_should_apply_all_migrations_once_and_reopen_cleanly() {
    let Some(database_url) = crate::support::test_env("CPR_TEST_DATABASE_URL") else {
        return;
    };
    let database = format!("cpr_store_migrator_{}", Uuid::new_v4().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect migration test PostgreSQL");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "create database \"{database}\""
    )))
    .execute(&admin)
    .await
    .expect("create migration test database");

    let isolated_url = PgConnectOptions::from_str(&database_url)
        .expect("parse migration test PostgreSQL URL")
        .database(&database)
        .to_url_lossy()
        .to_string();
    let pool_config = gateway_store::StorePoolConfig::default();
    let first = connect_and_migrate(&isolated_url, pool_config)
        .await
        .expect("apply migrations through production migrator");
    let session_settings = sqlx::query_as::<_, (String, i64, i64, i64)>(
        "select current_setting('application_name'),
                extract(epoch from current_setting('statement_timeout')::interval)::bigint,
                extract(epoch from current_setting('lock_timeout')::interval)::bigint,
                extract(epoch from current_setting(
                    'idle_in_transaction_session_timeout'
                )::interval)::bigint",
    )
    .fetch_one(&first)
    .await
    .expect("load runtime PostgreSQL session settings");
    let timezone: String = sqlx::query_scalar("show time zone")
        .fetch_one(&first)
        .await
        .unwrap();
    assert_eq!(
        timezone, "UTC",
        "runtime sessions use an explicit technical time basis"
    );
    let first_tables = sqlx::query_scalar::<_, String>(
        "select table_name
         from information_schema.tables
         where table_schema = 'public'
         order by table_name",
    )
    .fetch_all(&first)
    .await
    .expect("load migrated tables");
    first.close().await;

    let second = connect_and_migrate(&isolated_url, pool_config)
        .await
        .expect("reopen database through production migrator");
    let migration_count =
        sqlx::query_scalar::<_, i64>("select count(*) from _sqlx_migrations where success")
            .fetch_one(&second)
            .await
            .expect("count successful migrations");
    let response_id_types = sqlx::query_scalar::<_, String>(
        "select data_type
         from information_schema.columns
         where table_schema = 'public'
           and table_name = 'model_requests'
           and column_name in ('client_response_id', 'upstream_response_id')
         order by column_name",
    )
    .fetch_all(&second)
    .await
    .expect("load opaque response ID column types");
    let raw_response_id_index_exists = sqlx::query_scalar::<_, bool>(
        "select exists (
           select 1
           from pg_indexes
           where schemaname = 'public'
             and indexname = 'model_requests_client_response_uq'
         )",
    )
    .fetch_one(&second)
    .await
    .expect("check removed raw response ID index");
    let legacy_key_provider_column_exists = sqlx::query_scalar::<_, bool>(
        "select exists (
           select 1 from information_schema.columns
           where table_schema = 'public'
             and table_name = 'client_api_keys'
             and column_name = 'provider_kind'
         )",
    )
    .fetch_one(&second)
    .await
    .expect("check removed client key provider column");
    let routing_history_columns = sqlx::query_scalar::<_, String>(
        "select column_name from information_schema.columns
         where table_schema = 'public'
           and table_name = 'model_requests'
           and column_name in (
             'routing_scope', 'routing_group_refs', 'routing_group_names_snapshot'
           )
         order by column_name",
    )
    .fetch_all(&second)
    .await
    .expect("load routing history columns");
    second.close().await;

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database \"{database}\" with (force)"
    )))
    .execute(&admin)
    .await
    .expect("drop migration test database");
    admin.close().await;

    assert_eq!(
        first_tables,
        [
            "_sqlx_migrations",
            "account_group_accounts",
            "account_groups",
            "admin_audit_events",
            "admin_users",
            "backup_records",
            "backup_settings",
            "client_api_key_budget_windows",
            "client_api_key_charge_events",
            "client_api_key_groups",
            "client_api_keys",
            "model_request_leases",
            "model_request_observations",
            "model_requests",
            "openai_client_installations",
            "ops_events",
            "outbound_proxies",
            "plugin_account_group_resources",
            "plugin_artifact_credentials",
            "plugin_artifact_platforms",
            "plugin_artifacts",
            "plugin_client_api_key_resources",
            "plugin_instance_secrets",
            "plugin_instances",
            "plugin_source_credentials",
            "plugin_state_generations",
            "plugin_state_records",
            "plugin_update_sources",
            "plugin_version_configurations",
            "provider_accounts",
            "provider_authorization_receipts",
            "request_analytics_15m",
            "request_analytics_1d_counts",
            "request_analytics_1h",
            "request_analytics_build_state",
            "request_analytics_facts",
            "request_analytics_pending_buckets",
            "request_analytics_pending_requests",
            "runtime_settings",
        ]
    );
    assert_eq!(session_settings, ("codex-proxy-rs".to_owned(), 30, 5, 30));
    assert_eq!(
        migration_count,
        i64::try_from(TEST_MIGRATOR.iter().count())
            .expect("migration count fits PostgreSQL bigint")
    );
    assert_eq!(response_id_types, ["bytea", "bytea"]);
    assert!(!raw_response_id_index_exists);
    assert!(!legacy_key_provider_column_exists);
    assert!(routing_history_columns.is_empty());
}

#[test]
fn migrations_should_leave_transaction_ownership_to_sqlx() {
    let transaction_statements = TEST_MIGRATOR
        .iter()
        .flat_map(|migration| migration.sql.as_str().lines())
        .map(str::trim)
        .filter(|line| matches!(*line, "begin;" | "commit;"))
        .count();

    assert_eq!(transaction_statements, 0);
}
