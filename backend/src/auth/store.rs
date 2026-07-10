//! 管理员用户与会话存储。

use chrono::{DateTime, Duration, Utc};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use thiserror::Error;

use crate::infra::{identity::hash_credential, redis::RedisConnection};

/// 已持久化的管理员用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAdminUser {
    /// 管理员用户 ID。
    pub id: String,
    /// 管理员密码哈希。
    pub password_hash: String,
}

/// 管理认证存储错误。
#[derive(Debug, Error)]
pub enum AuthStoreError {
    #[error("PostgreSQL admin user operation failed: {0}")]
    Postgres(#[from] sqlx::Error),
    #[error("Redis admin session operation failed: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("invalid Redis admin session value: {0}")]
    Json(#[from] serde_json::Error),
}

pub type AuthStoreResult<T> = Result<T, AuthStoreError>;

/// PostgreSQL 管理员用户存储。
#[derive(Clone)]
pub struct PgAdminUserStore {
    pool: PgPool,
}

impl PgAdminUserStore {
    /// 构造存储。
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 如果还没有管理员用户，则创建默认管理员。
    pub async fn ensure_default_admin(&self, password_hash: &str) -> AuthStoreResult<bool> {
        let now = Utc::now();
        let result = sqlx::query(
            r"
insert into admin_users (id, password_hash, created_at, updated_at)
select $1, $2, $3, $3
where not exists (select 1 from admin_users)
on conflict do nothing",
        )
        .bind("admin_1")
        .bind(password_hash)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 读取第一个管理员用户。
    pub async fn load_first_admin(&self) -> AuthStoreResult<Option<StoredAdminUser>> {
        let row = sqlx::query_as::<_, (String, String)>(
            "select id, password_hash from admin_users order by created_at asc, id asc limit 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id, password_hash)| StoredAdminUser { id, password_hash }))
    }
}

/// Redis 管理员会话存储。
#[derive(Clone)]
pub struct RedisAdminSessionStore {
    redis: RedisConnection,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAdminSession {
    user_id: String,
    created_at: DateTime<Utc>,
}

impl RedisAdminSessionStore {
    pub fn new(redis: RedisConnection) -> Self {
        Self { redis }
    }

    pub async fn create_session(
        &self,
        session_id: &str,
        user_id: &str,
        ttl: Duration,
    ) -> AuthStoreResult<()> {
        let key = self.session_key(session_id);
        let value = serde_json::to_string(&StoredAdminSession {
            user_id: user_id.to_string(),
            created_at: Utc::now(),
        })?;
        let ttl_seconds = ttl.num_seconds().max(1) as u64;
        let mut connection = self.redis.manager();
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("EX")
            .arg(ttl_seconds)
            .query_async(&mut connection)
            .await?;
        Ok(())
    }

    pub async fn validate_session(&self, session_id: &str) -> AuthStoreResult<bool> {
        let mut connection = self.redis.manager();
        let value: Option<String> = connection.get(self.session_key(session_id)).await?;
        let Some(value) = value else {
            return Ok(false);
        };
        let session: StoredAdminSession = serde_json::from_str(&value)?;
        Ok(!session.user_id.trim().is_empty())
    }

    pub async fn delete_session(&self, session_id: &str) -> AuthStoreResult<bool> {
        let mut connection = self.redis.manager();
        let deleted: u64 = connection.del(self.session_key(session_id)).await?;
        Ok(deleted > 0)
    }

    fn session_key(&self, session_id: &str) -> String {
        self.redis
            .key(&format!("admin:session:{}", hash_credential(session_id)))
    }
}
