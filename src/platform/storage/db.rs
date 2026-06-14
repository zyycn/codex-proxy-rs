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
    ensure_session_affinity_schema(pool).await?;
    Ok(())
}

async fn ensure_account_usage_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("pragma table_info(account_usage)")
        .fetch_all(pool)
        .await?;
    for (name, sql) in ACCOUNT_USAGE_ADDED_COLUMNS {
        let exists = rows.iter().any(|row| row.get::<String, _>("name") == *name);
        if !exists {
            sqlx::query(*sql).execute(pool).await?;
        }
    }
    Ok(())
}

const ACCOUNT_USAGE_ADDED_COLUMNS: &[(&str, &str)] = &[
    (
        "empty_response_count",
        "alter table account_usage add column empty_response_count integer not null default 0",
    ),
    (
        "window_request_count",
        "alter table account_usage add column window_request_count integer not null default 0",
    ),
    (
        "window_input_tokens",
        "alter table account_usage add column window_input_tokens integer not null default 0",
    ),
    (
        "window_output_tokens",
        "alter table account_usage add column window_output_tokens integer not null default 0",
    ),
    (
        "window_cached_tokens",
        "alter table account_usage add column window_cached_tokens integer not null default 0",
    ),
    (
        "window_started_at",
        "alter table account_usage add column window_started_at text",
    ),
    (
        "window_reset_at",
        "alter table account_usage add column window_reset_at text",
    ),
    (
        "limit_window_seconds",
        "alter table account_usage add column limit_window_seconds integer",
    ),
];

async fn ensure_session_affinity_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("pragma table_info(session_affinities)")
        .fetch_all(pool)
        .await?;
    for (name, sql) in SESSION_AFFINITY_ADDED_COLUMNS {
        let exists = rows.iter().any(|row| row.get::<String, _>("name") == *name);
        if !exists {
            sqlx::query(*sql).execute(pool).await?;
        }
    }
    Ok(())
}

const SESSION_AFFINITY_ADDED_COLUMNS: &[(&str, &str)] = &[(
    "function_call_ids_json",
    "alter table session_affinities add column function_call_ids_json text not null default '[]'",
)];

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
