use std::{env, str::FromStr};

use gateway_store::postgres::connect_and_migrate;
use sqlx::{
    ConnectOptions as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

mod admin_security_audit;
mod admission_recovery;
mod client_keys;
mod config_catalog;
mod execution;
mod history;
mod observability;
mod ops_events;
mod provider_accounts;
mod retention;
mod runtime_settings;
mod snapshot;

pub(super) struct TestDatabase {
    admin: PgPool,
    pub(super) pool: PgPool,
    schema: String,
}

impl TestDatabase {
    pub(super) async fn create(label: &str) -> Option<Self> {
        let database_url = env::var("CPR_TEST_DATABASE_URL").ok()?;
        let schema = format!("cpr_store_{label}_{}", Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test PostgreSQL");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create schema \"{schema}\"")))
            .execute(&admin)
            .await
            .expect("create test schema");
        let search_path = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _metadata| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query("select set_config('search_path', $1, false)")
                        .bind(search_path)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect isolated test schema");
        let mut migration = pool.begin().await.expect("begin terminal migration");
        sqlx::raw_sql(include_str!("../../../../migrations/0001_initial.sql"))
            .execute(&mut *migration)
            .await
            .expect("apply terminal migration");
        migration.commit().await.expect("commit terminal migration");
        Some(Self {
            admin,
            pool,
            schema,
        })
    }

    pub(super) async fn close(self) {
        self.pool.close().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop schema \"{}\" cascade",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .expect("drop test schema");
        self.admin.close().await;
    }
}

#[tokio::test]
async fn connect_and_migrate_should_apply_0001_once_and_reopen_cleanly() {
    let Ok(database_url) = env::var("CPR_TEST_DATABASE_URL") else {
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
    let first = connect_and_migrate(&isolated_url)
        .await
        .expect("apply 0001 through production migrator");
    let first_table_count = sqlx::query_scalar::<_, i64>(
        "select count(*) from information_schema.tables where table_schema = 'public'",
    )
    .fetch_one(&first)
    .await
    .expect("count migrated tables");
    first.close().await;

    let second = connect_and_migrate(&isolated_url)
        .await
        .expect("reopen database through production migrator");
    let migration_count =
        sqlx::query_scalar::<_, i64>("select count(*) from _sqlx_migrations where success")
            .fetch_one(&second)
            .await
            .expect("count successful migrations");
    second.close().await;

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database \"{database}\" with (force)"
    )))
    .execute(&admin)
    .await
    .expect("drop migration test database");
    admin.close().await;

    assert_eq!((first_table_count, migration_count), (9, 1));
}

#[test]
fn initial_migration_should_leave_transaction_ownership_to_sqlx() {
    let transaction_statements = include_str!("../../../../migrations/0001_initial.sql")
        .lines()
        .map(str::trim)
        .filter(|line| matches!(*line, "begin;" | "commit;"))
        .count();

    assert_eq!(transaction_statements, 0);
}
