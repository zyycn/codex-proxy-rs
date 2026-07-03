//! 管理员会话存储与认证领域逻辑。

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::infra::identity::{hash_admin_password, verify_admin_password};

const LOGIN_FAILURE_WINDOW: Duration = Duration::minutes(15);
const LOGIN_LOCK_DURATION: Duration = Duration::minutes(15);
const LOGIN_MAX_FAILURES: u32 = 5;

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
    /// 管理员登录尝试过于频繁。
    #[error("too many admin login attempts")]
    LoginThrottled,
}

/// 管理员会话服务。
#[derive(Clone)]
pub struct AdminSessionService {
    store: SqliteAdminSessionStore,
    default_username: String,
    session_ttl_minutes: u64,
    login_failures: Arc<Mutex<HashMap<String, LoginFailureState>>>,
}

#[derive(Debug, Clone)]
struct LoginFailureState {
    failures: u32,
    first_failed_at: DateTime<Utc>,
    locked_until: Option<DateTime<Utc>>,
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
            login_failures: Arc::new(Mutex::new(HashMap::new())),
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
        source: &str,
        username: Option<&str>,
        password: &str,
    ) -> Result<Option<AdminLoginSession>, AdminSessionError> {
        let source = login_source_key(source);
        self.ensure_login_allowed(&source).await?;

        let username = username.unwrap_or(&self.default_username);
        if username != self.default_username.as_str() {
            self.record_failed_login(&source).await;
            return Ok(None);
        }
        let Some(admin) = self
            .store
            .load_first_admin()
            .await
            .map_err(|_| AdminSessionError::LoadAdmin)?
        else {
            self.record_failed_login(&source).await;
            return Ok(None);
        };
        let password_matches = verify_admin_password(password, &admin.password_hash)
            .map_err(|_| AdminSessionError::VerifyPassword)?;
        if !password_matches {
            self.record_failed_login(&source).await;
            return Ok(None);
        }
        self.clear_failed_login(&source).await;
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

    async fn ensure_login_allowed(&self, source: &str) -> Result<(), AdminSessionError> {
        let now = Utc::now();
        let mut failures = self.login_failures.lock().await;
        retain_active_login_failures(&mut failures, now);
        if failures
            .get(source)
            .and_then(|state| state.locked_until)
            .is_some_and(|locked_until| locked_until > now)
        {
            return Err(AdminSessionError::LoginThrottled);
        }
        Ok(())
    }

    async fn record_failed_login(&self, source: &str) {
        let now = Utc::now();
        let mut failures = self.login_failures.lock().await;
        retain_active_login_failures(&mut failures, now);
        let state = failures
            .entry(source.to_string())
            .or_insert_with(|| LoginFailureState {
                failures: 0,
                first_failed_at: now,
                locked_until: None,
            });
        if now - state.first_failed_at > LOGIN_FAILURE_WINDOW {
            state.failures = 0;
            state.first_failed_at = now;
            state.locked_until = None;
        }
        state.failures += 1;
        if state.failures >= LOGIN_MAX_FAILURES {
            state.locked_until = Some(now + LOGIN_LOCK_DURATION);
        }
    }

    async fn clear_failed_login(&self, source: &str) {
        self.login_failures.lock().await.remove(source);
    }
}

fn login_source_key(source: &str) -> String {
    let source = source.trim();
    if source.is_empty() {
        "unknown".to_string()
    } else {
        source.to_string()
    }
}

fn retain_active_login_failures(
    failures: &mut HashMap<String, LoginFailureState>,
    now: DateTime<Utc>,
) {
    failures.retain(|_, state| {
        state
            .locked_until
            .is_some_and(|locked_until| locked_until > now)
            || now - state.first_failed_at <= LOGIN_FAILURE_WINDOW
    });
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
