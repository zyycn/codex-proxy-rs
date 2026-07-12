//! 管理员认证领域服务。

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use tokio::sync::Mutex;

use crate::infra::identity::{
    generate_admin_session_token, hash_admin_password, verify_admin_password,
};

use super::{
    store::{PgAdminUserStore, RedisAdminSessionStore},
    types::{LoginSession, SessionError},
};

const LOGIN_FAILURE_WINDOW: Duration = Duration::minutes(15);
const LOGIN_LOCK_DURATION: Duration = Duration::minutes(15);
const LOGIN_MAX_FAILURES: u32 = 5;
const LOGIN_FAILURE_SOURCE_LIMIT: usize = 10_000;

/// 管理员会话服务。
#[derive(Clone)]
pub struct SessionService {
    users: PgAdminUserStore,
    sessions: RedisAdminSessionStore,
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

impl SessionService {
    /// 构造服务。
    pub fn new(
        users: PgAdminUserStore,
        sessions: RedisAdminSessionStore,
        default_username: String,
        session_ttl_minutes: u64,
    ) -> Self {
        Self {
            users,
            sessions,
            default_username,
            session_ttl_minutes,
            login_failures: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 校验管理员会话。
    pub async fn validate(&self, session_id: Option<&str>) -> Result<bool, SessionError> {
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        self.sessions
            .validate_session(session_id)
            .await
            .map_err(|_| SessionError::Validate)
    }

    /// 确保默认管理员存在。
    pub async fn ensure_default_admin(&self, password: &str) -> Result<bool, SessionError> {
        let password_hash =
            hash_admin_password(password).map_err(|_| SessionError::HashPassword)?;
        self.users
            .ensure_default_admin(&password_hash)
            .await
            .map_err(|_| SessionError::CreateAdmin)
    }

    /// 登录并创建管理员会话。
    pub async fn login(
        &self,
        source: &str,
        username: Option<&str>,
        password: &str,
    ) -> Result<Option<LoginSession>, SessionError> {
        let source = login_source_key(source);
        self.ensure_login_allowed(&source).await?;

        let username = username.unwrap_or(&self.default_username);
        if username != self.default_username.as_str() {
            self.record_failed_login(&source).await;
            return Ok(None);
        }
        let Some(admin) = self
            .users
            .load_first_admin()
            .await
            .map_err(|_| SessionError::LoadAdmin)?
        else {
            self.record_failed_login(&source).await;
            return Ok(None);
        };
        let password_matches = verify_admin_password(password, &admin.password_hash)
            .map_err(|_| SessionError::VerifyPassword)?;
        if !password_matches {
            self.record_failed_login(&source).await;
            return Ok(None);
        }
        self.clear_failed_login(&source).await;
        let session_id = generate_admin_session_token();
        let ttl_minutes = self.session_ttl_minutes.min(i64::MAX as u64) as i64;
        let expires_at = Utc::now() + Duration::minutes(ttl_minutes);
        self.sessions
            .create_session(
                &session_id,
                &admin.id,
                Duration::minutes(ttl_minutes.max(1)),
            )
            .await
            .map_err(|_| SessionError::CreateSession)?;
        Ok(Some(LoginSession {
            session_id,
            expires_at,
        }))
    }

    /// 删除管理员会话。
    pub async fn delete_session(&self, session_id: &str) -> Result<bool, SessionError> {
        self.sessions
            .delete_session(session_id)
            .await
            .map_err(|_| SessionError::DeleteSession)
    }

    async fn ensure_login_allowed(&self, source: &str) -> Result<(), SessionError> {
        let now = Utc::now();
        let login_locked = {
            let mut failures = self.login_failures.lock().await;
            retain_active_login_failures(&mut failures, now);
            if !failures.contains_key(source) && failures.len() >= LOGIN_FAILURE_SOURCE_LIMIT {
                return Err(SessionError::LoginThrottled);
            }
            failures
                .get(source)
                .and_then(|state| state.locked_until)
                .is_some_and(|locked_until| locked_until > now)
        };
        if login_locked {
            return Err(SessionError::LoginThrottled);
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
        drop(failures);
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
