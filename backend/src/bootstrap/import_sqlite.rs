//! 一次性 SQLite v3 到 PostgreSQL 终态库导入。

mod core;
mod telemetry;

use std::{collections::BTreeMap, fmt, path::Path};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    PgPool, Postgres, SqlitePool, Transaction,
};
use thiserror::Error;

const SQLITE_SCHEMA_VERSION: i64 = 3;

/// SQLite 历史库导入结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportSqliteReport {
    imported: BTreeMap<&'static str, u64>,
    normalized: BTreeMap<&'static str, u64>,
    discarded: BTreeMap<&'static str, u64>,
    cookie_expiration_parse_failures: u64,
}

impl ImportSqliteReport {
    fn add_imported(&mut self, table: &'static str) {
        *self.imported.entry(table).or_default() += 1;
    }

    fn add_discarded(&mut self, reason: &'static str, count: u64) {
        if count > 0 {
            *self.discarded.entry(reason).or_default() += count;
        }
    }

    fn add_normalized(&mut self, reason: &'static str) {
        *self.normalized.entry(reason).or_default() += 1;
    }

    fn add_cookie_expiration_parse_failure(&mut self) {
        self.cookie_expiration_parse_failures += 1;
    }

    /// 返回指定目标表导入的行数。
    pub fn imported_rows(&self, table: &str) -> u64 {
        self.imported.get(table).copied().unwrap_or_default()
    }

    /// 返回指定原因丢弃的行数。
    pub fn discarded_rows(&self, reason: &str) -> u64 {
        self.discarded.get(reason).copied().unwrap_or_default()
    }

    /// 返回指定规则规范化的行数。
    pub fn normalized_rows(&self, reason: &str) -> u64 {
        self.normalized.get(reason).copied().unwrap_or_default()
    }

    /// 返回无法解析并降级为会话 Cookie 的过期时间数量。
    pub fn cookie_expiration_parse_failures(&self) -> u64 {
        self.cookie_expiration_parse_failures
    }
}

impl fmt::Display for ImportSqliteReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "SQLite v3 import completed")?;
        for (table, count) in &self.imported {
            writeln!(formatter, "  imported {table}: {count}")?;
        }
        for (reason, count) in &self.normalized {
            writeln!(formatter, "  normalized {reason}: {count}")?;
        }
        for (reason, count) in &self.discarded {
            writeln!(formatter, "  discarded {reason}: {count}")?;
        }
        write!(
            formatter,
            "  cookie expiration parse failures: {}",
            self.cookie_expiration_parse_failures
        )
    }
}

/// SQLite 历史库导入错误。
#[derive(Debug, Error)]
pub enum ImportSqliteError {
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("unsupported SQLite schema version {actual:?}; expected 3")]
    UnsupportedSourceVersion { actual: Option<i64> },
    #[error("target PostgreSQL database is not empty")]
    TargetNotEmpty,
    #[error("invalid timestamp in {table}.{column}: {value}")]
    InvalidTimestamp {
        table: &'static str,
        column: &'static str,
        value: String,
    },
    #[error("invalid JSON in {table}.{column}: {source}")]
    InvalidJson {
        table: &'static str,
        column: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// 将只读 SQLite v3 历史库一次性导入空的 PostgreSQL 终态库。
pub async fn import_sqlite(
    target: &PgPool,
    source_path: impl AsRef<Path>,
) -> Result<ImportSqliteReport, ImportSqliteError> {
    let options = SqliteConnectOptions::new()
        .filename(source_path.as_ref())
        .read_only(true)
        .create_if_missing(false);
    let source = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    validate_source_version(&source).await?;

    let mut transaction = target.begin().await?;
    reject_nonempty_target(&mut transaction).await?;

    let mut report = ImportSqliteReport::default();
    core::import_core_tables(&source, &mut transaction, &mut report).await?;
    telemetry::import_telemetry_tables(&source, &mut transaction, &mut report).await?;
    core::count_discarded_runtime_tables(&source, &mut report).await?;

    transaction.commit().await?;
    source.close().await;
    Ok(report)
}

async fn validate_source_version(source: &SqlitePool) -> Result<(), ImportSqliteError> {
    let (actual,): (Option<i64>,) = sqlx::query_as("select max(version) from schema_migrations")
        .fetch_one(source)
        .await?;
    if actual != Some(SQLITE_SCHEMA_VERSION) {
        return Err(ImportSqliteError::UnsupportedSourceVersion { actual });
    }
    Ok(())
}

async fn reject_nonempty_target(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), ImportSqliteError> {
    let (rows,): (i64,) = sqlx::query_as(
        "select
           (select count(*) from admin_users)
         + (select count(*) from client_api_keys)
         + (select count(*) from runtime_settings)
         + (select count(*) from accounts)
         + (select count(*) from account_usage)
         + (select count(*) from account_model_usage)
         + (select count(*) from usage_records)
         + (select count(*) from ops_error_logs)
         + (select count(*) from request_time_buckets)
         + (select count(*) from account_cookies)
         + (select count(*) from fingerprints)
         + (select count(*) from fingerprint_update_history)",
    )
    .fetch_one(&mut **transaction)
    .await?;
    if rows != 0 {
        return Err(ImportSqliteError::TargetNotEmpty);
    }
    Ok(())
}

fn parse_timestamp(
    table: &'static str,
    column: &'static str,
    value: String,
) -> Result<DateTime<Utc>, ImportSqliteError> {
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| ImportSqliteError::InvalidTimestamp {
            table,
            column,
            value,
        })
}

fn parse_optional_timestamp(
    table: &'static str,
    column: &'static str,
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, ImportSqliteError> {
    value
        .map(|value| parse_timestamp(table, column, value))
        .transpose()
}

fn parse_cookie_expiration(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_rfc2822(value))
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn parse_json(
    table: &'static str,
    column: &'static str,
    value: &str,
) -> Result<Value, ImportSqliteError> {
    serde_json::from_str(value).map_err(|source| ImportSqliteError::InvalidJson {
        table,
        column,
        source,
    })
}

fn parse_optional_json(
    table: &'static str,
    column: &'static str,
    value: Option<String>,
) -> Result<Option<Value>, ImportSqliteError> {
    value
        .map(|value| parse_json(table, column, &value))
        .transpose()
}

fn normalized_dimension(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "__unknown__".to_string())
}
