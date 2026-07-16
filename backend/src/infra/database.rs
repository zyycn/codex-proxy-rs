//! PostgreSQL 连接与迁移初始化。

use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgPoolOptions};
use tracing::info;

const MIGRATION_LOCK_KEY: i64 = i64::from_be_bytes(*b"CPR-MIGR");

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "cache_write_tokens",
        sql: include_str!("migrations/0002_cache_write_tokens.sql"),
    },
];

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

struct AppliedMigration {
    version: i64,
    name: String,
    checksum: String,
}

impl Migration {
    fn checksum(&self) -> String {
        hex::encode(Sha256::digest(self.sql.as_bytes()))
    }
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
    validate_migration_catalog()?;

    let mut tx = pool.begin().await?;
    let migration_table_created = ensure_migrations_table(&mut tx).await?;

    let applied = applied_migrations(&mut tx).await?;
    if applied.is_empty() {
        reject_unversioned_existing_schema(&mut tx).await?;
    }
    validate_applied_migrations(&applied)?;

    for migration in MIGRATIONS.iter().skip(applied.len()) {
        sqlx::raw_sql(migration.sql).execute(&mut *tx).await?;
        record_migration(&mut tx, migration).await?;
    }

    tx.commit().await?;

    if migration_table_created {
        info!("数据库迁移表已创建");
    } else {
        info!("数据库迁移表已存在，跳过创建");
    }
    Ok(())
}

fn validate_migration_catalog() -> Result<(), sqlx::Error> {
    let mut previous = 0;
    for migration in MIGRATIONS {
        if migration.version <= previous {
            return Err(sqlx::Error::Protocol(format!(
                "PostgreSQL migration versions must be strictly increasing: {} follows {previous}",
                migration.version
            )));
        }
        if migration.name.trim().is_empty() {
            return Err(sqlx::Error::Protocol(format!(
                "PostgreSQL migration {} has an empty name",
                migration.version
            )));
        }
        previous = migration.version;
    }
    Ok(())
}

async fn ensure_migrations_table(tx: &mut Transaction<'_, Postgres>) -> Result<bool, sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut **tx)
        .await?;

    let exists = sqlx::query_scalar(
        "select exists (
          select 1
          from information_schema.tables
          where table_schema = current_schema()
            and table_name = 'schema_migrations'
            and table_type = 'BASE TABLE'
        )",
    )
    .fetch_one(&mut **tx)
    .await?;
    if exists {
        return Ok(false);
    }

    sqlx::query(
        "create table schema_migrations (
          version bigint not null check (version > 0),
          name text not null,
          checksum text not null,
          applied_at timestamptz not null default now(),
          primary key (version)
        )",
    )
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

async fn applied_migrations(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<AppliedMigration>, sqlx::Error> {
    let rows =
        sqlx::query("select version, name, checksum from schema_migrations order by version")
            .fetch_all(&mut **tx)
            .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppliedMigration {
            version: row.get("version"),
            name: row.get("name"),
            checksum: row.get("checksum"),
        })
        .collect())
}

fn validate_applied_migrations(applied: &[AppliedMigration]) -> Result<(), sqlx::Error> {
    if applied.len() > MIGRATIONS.len() {
        return Err(unsupported_schema_version(
            applied.last().map_or(0, |migration| migration.version),
        ));
    }

    for (index, applied_migration) in applied.iter().enumerate() {
        let expected = &MIGRATIONS[index];
        if applied_migration.version != expected.version {
            return Err(unsupported_schema_version(applied_migration.version));
        }
        if applied_migration.name != expected.name {
            return Err(sqlx::Error::Protocol(format!(
                "PostgreSQL migration {} name mismatch: database has {:?}, binary expects {:?}",
                expected.version, applied_migration.name, expected.name
            )));
        }

        let expected_checksum = expected.checksum();
        if applied_migration.checksum != expected_checksum {
            return Err(sqlx::Error::Protocol(format!(
                "PostgreSQL migration {} checksum mismatch",
                expected.version
            )));
        }
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
        "insert into schema_migrations (version, name, checksum, applied_at)
         values ($1, $2, $3, now())",
    )
    .bind(migration.version)
    .bind(migration.name)
    .bind(migration.checksum())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn current_schema_version() -> i64 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

fn unsupported_schema_version(version: i64) -> sqlx::Error {
    sqlx::Error::Protocol(format!(
        "unsupported PostgreSQL schema version {version}; expected {}",
        current_schema_version()
    ))
}
