//! PostgreSQL 连接与迁移初始化。

use std::time::Duration;

use sqlx::{postgres::PgPoolOptions, PgPool, Postgres, Transaction};

const CURRENT_SCHEMA_VERSION: i64 = 1;
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial",
    sql: include_str!("migrations/0001_initial.sql"),
}];

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

/// 连接 PostgreSQL 并应用未执行的迁移。
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// 检查 PostgreSQL 连接是否可用。
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("select 1").execute(pool).await.map(|_| ())
}

/// 将 PostgreSQL schema 推进到当前版本。
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    ensure_migrations_table(&mut tx).await?;

    let applied_versions = applied_migration_versions(&mut tx).await?;
    if applied_versions.is_empty() {
        reject_unversioned_existing_schema(&mut tx).await?;
    }
    validate_applied_migrations(&applied_versions)?;

    for migration in MIGRATIONS {
        if applied_versions.contains(&migration.version) {
            continue;
        }
        sqlx::raw_sql(migration.sql).execute(&mut *tx).await?;
        record_migration(&mut tx, migration).await?;
    }

    tx.commit().await
}

async fn ensure_migrations_table(tx: &mut Transaction<'_, Postgres>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "create table if not exists schema_migrations (
          version bigint not null check (version > 0),
          name text not null,
          applied_at timestamptz not null default now(),
          primary key (version)
        )",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn applied_migration_versions(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<i64>, sqlx::Error> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("select version from schema_migrations order by version")
            .fetch_all(&mut **tx)
            .await?;
    Ok(rows.into_iter().map(|row| row.0).collect())
}

fn validate_applied_migrations(applied_versions: &[i64]) -> Result<(), sqlx::Error> {
    for version in applied_versions {
        if !MIGRATIONS
            .iter()
            .any(|migration| migration.version == *version)
        {
            return Err(unsupported_schema_version(*version));
        }
    }
    if let Some(version) = applied_versions
        .last()
        .copied()
        .filter(|version| *version > CURRENT_SCHEMA_VERSION)
    {
        return Err(unsupported_schema_version(version));
    }
    Ok(())
}

async fn reject_unversioned_existing_schema(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), sqlx::Error> {
    let (table_count,): (i64,) = sqlx::query_as(
        "select count(*)
         from information_schema.tables
         where table_schema = current_schema()
           and table_type = 'BASE TABLE'
           and table_name <> 'schema_migrations'",
    )
    .fetch_one(&mut **tx)
    .await?;
    if table_count > 0 {
        return Err(sqlx::Error::Protocol(
            "unversioned PostgreSQL schema is unsupported; use an empty database".to_string(),
        ));
    }
    Ok(())
}

async fn record_migration(
    tx: &mut Transaction<'_, Postgres>,
    migration: &Migration,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into schema_migrations (version, name, applied_at)
         values ($1, $2, now())",
    )
    .bind(migration.version)
    .bind(migration.name)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn unsupported_schema_version(version: i64) -> sqlx::Error {
    sqlx::Error::Protocol(format!(
        "unsupported PostgreSQL schema version {version}; expected {CURRENT_SCHEMA_VERSION}"
    ))
}
