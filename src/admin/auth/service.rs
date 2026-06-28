//! 管理员会话存储与认证领域逻辑。

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use thiserror::Error;
use uuid::Uuid;

use crate::infra::identity::{hash_admin_password, verify_admin_password};

/// 管理员登录成功后的会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLoginSession {
    /// 会话 ID。
    pub session_id: String,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
}

/// 管理员会话错误。
#[derive(Debug, Error)]
pub enum AdminSessionError {
    /// 校验管理员会话失败。
    #[error("failed to validate admin session")]
    Validate,
    /// 计算管理员密码哈希失败。
    #[error("failed to hash admin password")]
    HashPassword,
    /// 创建默认管理员失败。
    #[error("failed to create default admin user")]
    CreateAdmin,
    /// 读取管理员用户失败。
    #[error("failed to load admin user")]
    LoadAdmin,
    /// 校验管理员密码失败。
    #[error("failed to verify admin password")]
    VerifyPassword,
    /// 创建管理员会话失败。
    #[error("failed to create admin session")]
    CreateSession,
    /// 删除管理员会话失败。
    #[error("failed to delete admin session")]
    DeleteSession,
}

/// 管理员会话服务。
#[derive(Clone)]
pub struct AdminSessionService {
    store: SqliteAdminSessionStore,
    default_username: String,
    session_ttl_minutes: u64,
}

impl AdminSessionService {
    /// 构造服务。
    pub fn new(
        store: SqliteAdminSessionStore,
        default_username: String,
        session_ttl_minutes: u64,
    ) -> Self {
        Self {
            store,
            default_username,
            session_ttl_minutes,
        }
    }

    /// 校验管理员会话。
    pub async fn validate(&self, session_id: Option<&str>) -> Result<bool, AdminSessionError> {
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        self.store
            .validate_session(session_id)
            .await
            .map_err(|_| AdminSessionError::Validate)
    }

    /// 确保默认管理员存在。
    pub async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminSessionError> {
        let password_hash =
            hash_admin_password(password).map_err(|_| AdminSessionError::HashPassword)?;
        self.store
            .ensure_default_admin(&password_hash)
            .await
            .map_err(|_| AdminSessionError::CreateAdmin)
    }

    /// 登录并创建管理员会话。
    pub async fn login(
        &self,
        username: Option<&str>,
        password: &str,
    ) -> Result<Option<AdminLoginSession>, AdminSessionError> {
        let username = username.unwrap_or(&self.default_username);
        if username != self.default_username.as_str() {
            return Ok(None);
        }
        let Some(admin) = self
            .store
            .load_first_admin()
            .await
            .map_err(|_| AdminSessionError::LoadAdmin)?
        else {
            return Ok(None);
        };
        let password_matches = verify_admin_password(password, &admin.password_hash)
            .map_err(|_| AdminSessionError::VerifyPassword)?;
        if !password_matches {
            return Ok(None);
        }
        let session_id = format!("sess_{}", Uuid::new_v4().simple());
        let ttl_minutes = self.session_ttl_minutes.min(i64::MAX as u64) as i64;
        let expires_at = Utc::now() + Duration::minutes(ttl_minutes);
        self.store
            .create_session(&session_id, &admin.id, expires_at)
            .await
            .map_err(|_| AdminSessionError::CreateSession)?;
        Ok(Some(AdminLoginSession {
            session_id,
            expires_at,
        }))
    }

    /// 删除管理员会话。
    pub async fn delete_session(&self, session_id: &str) -> Result<bool, AdminSessionError> {
        self.store
            .delete_session(session_id)
            .await
            .map_err(|_| AdminSessionError::DeleteSession)
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
