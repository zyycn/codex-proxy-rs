use std::{str::FromStr, time::Duration};

use gateway_store::postgres::{
    ObservabilityQueryBudget, PgAdminAccountStore, connect_and_migrate,
};
use sqlx::{
    ConnectOptions as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

mod account_groups;
mod admin_security_audit;
mod admission_recovery;
mod backup;
mod client_keys;
mod execution;
mod execution_buffer;
mod health;
mod observability;
mod ops_events;
mod provider_accounts;
mod query_budget;
mod retention;
mod runtime_settings;
mod snapshot;
mod snapshots;

static TEST_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

pub(super) struct TestDatabase {
    admin: PgPool,
    pub(super) pool: PgPool,
    schema: String,
}

pub(super) fn observability_query_budget() -> ObservabilityQueryBudget {
    ObservabilityQueryBudget::try_new(4, Duration::from_secs(1))
        .expect("valid test observability query budget")
}

pub(super) fn admin_account_store(pool: &PgPool) -> PgAdminAccountStore {
    PgAdminAccountStore::new(pool.clone(), None, observability_query_budget())
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
        TEST_MIGRATOR
            .run(&pool)
            .await
            .expect("apply test migrations");
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
            "client_api_key_groups",
            "client_api_keys",
            "model_requests",
            "ops_events",
            "provider_accounts",
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
    assert_eq!(
        routing_history_columns,
        [
            "routing_group_names_snapshot",
            "routing_group_refs",
            "routing_scope",
        ]
    );
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
