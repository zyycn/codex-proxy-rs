use std::str::FromStr;

use gateway_store::postgres::connect_and_migrate;
use sqlx::{
    ConnectOptions as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

mod admin_security_audit;
mod admission_recovery;
mod backup;
mod client_keys;
mod execution;
mod execution_buffer;
mod observability;
mod ops_events;
mod provider_accounts;
mod retention;
mod runtime_settings;
mod snapshot;
mod snapshots;

pub(super) struct TestDatabase {
    admin: PgPool,
    pub(super) pool: PgPool,
    schema: String,
}

impl TestDatabase {
    pub(super) async fn create(label: &str) -> Option<Self> {
        let database_url = crate::support::test_env("CPR_TEST_DATABASE_URL")?;
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
        sqlx::raw_sql(include_str!(
            "../../../../migrations/0002_snapshot_provider_account_identity.sql"
        ))
        .execute(&mut *migration)
        .await
        .expect("apply snapshot migration");
        sqlx::raw_sql(include_str!("../../../../migrations/0003_s3_backup.sql"))
            .execute(&mut *migration)
            .await
            .expect("apply backup migration");
        sqlx::raw_sql(include_str!(
            "../../../../migrations/0004_drop_provider_accounts_id_kind_uq.sql"
        ))
        .execute(&mut *migration)
        .await
        .expect("apply index drop migration");
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
    let first = connect_and_migrate(&isolated_url, gateway_store::StorePoolConfig::default())
        .await
        .expect("apply migrations through production migrator");
    let first_table_count = sqlx::query_scalar::<_, i64>(
        "select count(*) from information_schema.tables where table_schema = 'public'",
    )
    .fetch_one(&first)
    .await
    .expect("count migrated tables");
    first.close().await;

    let second = connect_and_migrate(&isolated_url, gateway_store::StorePoolConfig::default())
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
    second.close().await;

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database \"{database}\" with (force)"
    )))
    .execute(&admin)
    .await
    .expect("drop migration test database");
    admin.close().await;

    assert_eq!((first_table_count, migration_count), (10, 4));
    assert_eq!(response_id_types, ["bytea", "bytea"]);
    assert!(!raw_response_id_index_exists);
}

#[test]
fn migrations_should_leave_transaction_ownership_to_sqlx() {
    let transaction_statements = [
        include_str!("../../../../migrations/0001_initial.sql"),
        include_str!("../../../../migrations/0002_snapshot_provider_account_identity.sql"),
        include_str!("../../../../migrations/0003_s3_backup.sql"),
        include_str!("../../../../migrations/0004_drop_provider_accounts_id_kind_uq.sql"),
    ]
    .into_iter()
    .flat_map(str::lines)
    .map(str::trim)
    .filter(|line| matches!(*line, "begin;" | "commit;"))
    .count();

    assert_eq!(transaction_statements, 0);
}
