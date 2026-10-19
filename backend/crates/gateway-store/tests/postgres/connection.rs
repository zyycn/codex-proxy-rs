//! PostgreSQL 连接初始化、会话设置与事务取消测试

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
async fn connection_initialization_preserves_settings_when_reopened() {
    let Some(database_url) = crate::support::test_env("CPR_TEST_DATABASE_URL") else {
        return;
    };
    let database = format!("cpr_store_connection_{}", Uuid::new_v4().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test PostgreSQL");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "create database \"{database}\""
    )))
    .execute(&admin)
    .await
    .expect("create connection test database");

    let isolated_url = PgConnectOptions::from_str(&database_url)
        .expect("parse test PostgreSQL URL")
        .database(&database)
        .to_url_lossy()
        .to_string();
    let pool_config = gateway_store::StorePoolConfig::default();
    let first = connect_and_migrate(&isolated_url, pool_config)
        .await
        .expect("initialize production connection");
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
    sqlx::query("update runtime_settings set refresh_margin_seconds = 1800 where id = 1")
        .execute(&first)
        .await
        .expect("save custom runtime setting");
    first.close().await;

    let second = connect_and_migrate(&isolated_url, pool_config)
        .await
        .expect("reopen production connection");
    let refresh_margin: i64 =
        sqlx::query_scalar("select refresh_margin_seconds from runtime_settings where id = 1")
            .fetch_one(&second)
            .await
            .expect("load saved runtime setting");
    second.close().await;

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database \"{database}\" with (force)"
    )))
    .execute(&admin)
    .await
    .expect("drop connection test database");
    admin.close().await;

    assert_eq!(session_settings, ("codex-proxy-rs".to_owned(), 30, 5, 30));
    assert_eq!(refresh_margin, 1800, "reopening preserves custom settings");
}
