//! 管理员会话存储与认证领域逻辑。

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

// ---------------------------------------------------------------------------
// 领域类型
// ---------------------------------------------------------------------------

/// 管理员会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSession {
    /// 会话 ID。
    pub id: String,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
}

impl AdminSession {
    /// 判断会话在给定时间是否已经过期。
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// 根据 TTL 计算会话过期时间。
pub fn session_expiry(now: DateTime<Utc>, ttl_minutes: i64) -> DateTime<Utc> {
    now + Duration::minutes(ttl_minutes.max(0))
}

// ---------------------------------------------------------------------------
// 管理员登录
// ---------------------------------------------------------------------------

/// 管理员登录请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLoginRequest {
    /// 用户名。
    pub username: String,
    /// 明文密码。
    pub password: String,
}

/// 管理员认证服务。
#[derive(Debug, Clone)]
pub struct AdminAuthService {
    default_username: String,
}

impl AdminAuthService {
    /// 构造管理员认证服务。
    pub fn new(default_username: impl Into<String>) -> Self {
        Self {
            default_username: default_username.into(),
        }
    }

    /// 判断用户名是否匹配配置中的管理员用户名。
    pub fn username_matches(&self, username: &str) -> bool {
        self.default_username == username
    }
}

// ---------------------------------------------------------------------------
// 已持久化的管理用户
// ---------------------------------------------------------------------------

/// 已持久化的管理员用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAdminUser {
    /// 管理员用户 ID。
    pub id: String,
    /// 管理员密码哈希。
    pub password_hash: String,
}

// ---------------------------------------------------------------------------
// SQLite 管理员会话存储
// ---------------------------------------------------------------------------

/// SQLite 管理员会话存储结果。
pub type SqliteAdminSessionStoreResult<T> = Result<T, sqlx::Error>;

/// SQLite 管理员会话存储。
#[derive(Clone)]
pub struct SqliteAdminSessionStore {
    pool: SqlitePool,
}

impl SqliteAdminSessionStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 如果还没有管理员用户，则创建默认管理员。
    pub async fn ensure_default_admin(
        &self,
        password_hash: &str,
    ) -> SqliteAdminSessionStoreResult<bool> {
        let existing: (i64,) = sqlx::query_as("select count(*) from admin_users")
            .fetch_one(&self.pool)
            .await?;
        if existing.0 > 0 {
            return Ok(false);
        }

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
        )
        .bind("admin_1")
        .bind(password_hash)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        Ok(true)
    }

    /// 读取第一个管理员用户。
    pub async fn load_first_admin(&self) -> SqliteAdminSessionStoreResult<Option<StoredAdminUser>> {
        let row = sqlx::query_as::<_, (String, String)>(
            "select id, password_hash from admin_users order by created_at asc, id asc limit 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id, password_hash)| StoredAdminUser { id, password_hash }))
    }

    /// 创建管理员会话。
    pub async fn create_session(
        &self,
        session_id: &str,
        user_id: &str,
        expires_at: DateTime<Utc>,
    ) -> SqliteAdminSessionStoreResult<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(expires_at.to_rfc3339())
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 校验管理员会话是否存在且未过期。
    pub async fn validate_session(&self, session_id: &str) -> SqliteAdminSessionStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query("select 1 from admin_sessions where id = ? and expires_at > ?")
            .bind(session_id)
            .bind(now)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// 删除已经过期的管理员会话。
    pub async fn cleanup_expired_sessions(
        &self,
        now: DateTime<Utc>,
    ) -> SqliteAdminSessionStoreResult<u64> {
        let result = sqlx::query("delete from admin_sessions where expires_at <= ?")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 删除指定的管理员会话。
    pub async fn delete_session(&self, session_id: &str) -> SqliteAdminSessionStoreResult<bool> {
        let result = sqlx::query("delete from admin_sessions where id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
