//! SQLite 数据库连接与迁移初始化。

use std::{fs, path::Path, str::FromStr, time::Duration};

use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};

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

/// 连接 SQLite 并应用未执行的迁移。
pub async fn connect_sqlite(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    ensure_sqlite_parent_dir(database_url)?;
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePool::connect_with(options).await?;

    initialize_schema(&pool).await?;

    Ok(pool)
}

async fn initialize_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
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

    tx.commit().await?;
    Ok(())
}

async fn ensure_migrations_table(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    if table_exists(tx, "schema_migrations").await? {
        return Ok(());
    }

    sqlx::query(
        "create table schema_migrations (
          version integer not null check (version > 0),
          name text not null,
          applied_at text not null,
          primary key (version)
        )",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn table_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table_name: &str,
) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(
        "select 1
         from sqlite_master
         where type = 'table' and name = ?
         limit 1",
    )
    .bind(table_name)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.is_some())
}

async fn applied_migration_versions(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<Vec<i64>, sqlx::Error> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("select version from schema_migrations order by version")
            .fetch_all(&mut **tx)
            .await?;
    Ok(rows.into_iter().map(|row| row.0).collect())
}

fn validate_applied_migrations(applied_versions: &[i64]) -> Result<(), sqlx::Error> {
    for version in applied_versions {
        if MIGRATIONS
            .iter()
            .any(|migration| migration.version == *version)
        {
            continue;
        }
        return Err(unsupported_schema_version(*version));
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
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let (table_count,): (i64,) = sqlx::query_as(
        "select count(*)
           from sqlite_master
           where type = 'table'
             and name <> 'schema_migrations'
             and name not like 'sqlite_%'",
    )
    .fetch_one(&mut **tx)
    .await?;
    if table_count > 0 {
        return Err(sqlx::Error::Protocol(
            "unversioned sqlite schema is unsupported; recreate the database or add a migration"
                .to_string(),
        ));
    }
    Ok(())
}

async fn record_migration(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    migration: &Migration,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into schema_migrations (version, name, applied_at)
         values (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .bind(migration.version)
    .bind(migration.name)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn unsupported_schema_version(version: i64) -> sqlx::Error {
    sqlx::Error::Protocol(format!(
        "unsupported sqlite schema version {version}; expected {CURRENT_SCHEMA_VERSION}"
    ))
}

fn ensure_sqlite_parent_dir(database_url: &str) -> Result<(), sqlx::Error> {
    let Some(path) = sqlite_file_path(database_url) else {
        return Ok(());
    };
    let Some(parent) = Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(sqlx::Error::Io)
}

fn sqlite_file_path(database_url: &str) -> Option<&str> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))?
        .split_once('?')
        .map_or_else(
            || {
                database_url
                    .strip_prefix("sqlite://")
                    .or_else(|| database_url.strip_prefix("sqlite:"))
                    .unwrap_or_default()
            },
            |(path, _)| path,
        );
    (!path.is_empty() && path != ":memory:").then_some(path)
}
