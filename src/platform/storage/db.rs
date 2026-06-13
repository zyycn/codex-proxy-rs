use std::{fs, path::Path, str::FromStr, time::Duration};

use sqlx::{sqlite::SqliteConnectOptions, Row, SqlitePool};

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
    let schema = include_str!("schema.sql");
    sqlx::raw_sql(schema).execute(pool).await?;
    ensure_account_usage_schema(pool).await?;
    Ok(())
}

async fn ensure_account_usage_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("pragma table_info(account_usage)")
        .fetch_all(pool)
        .await?;
    let exists = rows
        .iter()
        .any(|row| row.get::<String, _>("name") == "empty_response_count");
    if !exists {
        sqlx::query(
            "alter table account_usage add column empty_response_count integer not null default 0",
        )
        .execute(pool)
        .await?;
    }
    Ok(())
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
