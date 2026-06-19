//! SQLite 刷新租约存储。

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use thiserror::Error;

/// SQLite 刷新租约存储结果。
pub type SqliteRefreshLeaseStoreResult<T> = Result<T, SqliteRefreshLeaseStoreError>;

/// SQLite 刷新租约存储错误。
#[derive(Debug, Error)]
pub enum SqliteRefreshLeaseStoreError {
    /// SQLite 操作失败。
    #[error("sqlite refresh lease query failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// SQLite 刷新租约存储。
#[derive(Clone)]
pub struct SqliteRefreshLeaseStore {
    pool: SqlitePool,
}

impl SqliteRefreshLeaseStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 尝试获取账号刷新租约。
    pub async fn try_acquire(
        &self,
        account_id: &str,
        owner: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> SqliteRefreshLeaseStoreResult<bool> {
        let result = sqlx::query(
            r#"
insert into account_refresh_leases (account_id, owner, expires_at, updated_at)
values (?, ?, ?, ?)
on conflict(account_id) do update set
  owner = excluded.owner,
  expires_at = excluded.expires_at,
  updated_at = excluded.updated_at
where account_refresh_leases.expires_at <= ?
  or account_refresh_leases.owner = ?
"#,
        )
        .bind(account_id)
        .bind(owner)
        .bind(expires_at.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 释放账号刷新租约。
    pub async fn release(
        &self,
        account_id: &str,
        owner: &str,
    ) -> SqliteRefreshLeaseStoreResult<bool> {
        let result =
            sqlx::query("delete from account_refresh_leases where account_id = ? and owner = ?")
                .bind(account_id)
                .bind(owner)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }
}
