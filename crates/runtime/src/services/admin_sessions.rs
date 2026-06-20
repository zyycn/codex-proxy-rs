use super::*;

/// 管理员会话服务。
#[derive(Clone)]
pub struct AdminSessionService {
    store: SqliteAdminSessionStore,
    auth: AdminAuthService,
    default_username: String,
    session_ttl_minutes: u64,
}

impl AdminSessionService {
    /// 构造管理员会话服务。
    pub fn new(
        store: SqliteAdminSessionStore,
        default_username: String,
        session_ttl_minutes: u64,
    ) -> Self {
        Self {
            store,
            auth: AdminAuthService::new(default_username.clone()),
            default_username,
            session_ttl_minutes,
        }
    }

    /// 校验管理员会话是否存在且未过期。
    pub async fn validate(&self, session_id: Option<&str>) -> Result<bool, AdminSessionError> {
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        self.store
            .validate_session(session_id)
            .await
            .map_err(|_| AdminSessionError::Validate)
    }

    /// 如果还没有管理员用户，则根据配置密码创建默认管理员。
    pub async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminSessionError> {
        let password_hash =
            hash_admin_password(password).map_err(|_| AdminSessionError::HashPassword)?;
        self.store
            .ensure_default_admin(&password_hash)
            .await
            .map_err(|_| AdminSessionError::CreateAdmin)
    }

    /// 使用管理员用户名和密码创建会话。
    pub async fn login(
        &self,
        username: Option<&str>,
        password: &str,
    ) -> Result<Option<AdminLoginSession>, AdminSessionError> {
        let username = username.unwrap_or(&self.default_username);
        if !self.auth.username_matches(username) {
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

        let session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
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
}

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
    /// 会话校验失败。
    #[error("failed to validate admin session")]
    Validate,
    /// 密码哈希失败。
    #[error("failed to hash admin password")]
    HashPassword,
    /// 创建管理员失败。
    #[error("failed to create default admin user")]
    CreateAdmin,
    /// 读取管理员失败。
    #[error("failed to load admin user")]
    LoadAdmin,
    /// 密码校验失败。
    #[error("failed to verify admin password")]
    VerifyPassword,
    /// 创建会话失败。
    #[error("failed to create admin session")]
    CreateSession,
}
