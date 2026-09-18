//! 控制面登录、会话恢复与身份校验的唯一 owner。

use std::{net::IpAddr, sync::Arc, time::Duration as StdDuration};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use gateway_core::engine::execution::ClientKeyVerifier;
use rand_core::{OsRng, RngCore as _};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::{
    model::{
        AdminError, AdminErrorKind,
        auth::{
            AdminAuditEvent, AuditActorKind, AuthSession, ChangePassword, LoginCommand, LoginError,
            LoginResult, SessionSubject,
        },
    },
    ports::store::AuthStore,
};

use super::map_store_error;

/// 所有控制面接口消费同一个会话服务，权限由服务端身份决定。
#[async_trait]
pub trait AuthService: Send + Sync {
    async fn change_password(
        &self,
        session_id: Option<&str>,
        command: ChangePassword,
        source_ip: IpAddr,
    ) -> Result<(), AdminError>;
    async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminError>;
    async fn session(&self, session_id: Option<&str>) -> Result<Option<AuthSession>, AdminError>;
    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminError>;
    async fn verify_admin_api_key(&self, key: &str) -> Result<bool, AdminError>;
    async fn login(
        &self,
        command: LoginCommand,
        source_ip: IpAddr,
        previous_session_id: Option<&str>,
    ) -> Result<LoginResult, LoginError>;
    async fn logout(&self, session_id: &str) -> Result<(), AdminError>;
}

const MAX_SESSION_TTL_MINUTES: i64 = 366 * 24 * 60;
const LOGIN_WINDOW: StdDuration = StdDuration::from_secs(60);
const LOGIN_ATTEMPTS_PER_SOURCE: u32 = 10;
const LOGIN_ATTEMPTS_GLOBAL: u32 = 200;

pub(crate) struct DefaultAuthService {
    default_admin_user_id: String,
    admin_session_ttl: Duration,
    key_session_ttl: Duration,
    store: Arc<dyn AuthStore>,
    verifier: Arc<dyn ClientKeyVerifier>,
}

impl DefaultAuthService {
    #[must_use]
    pub(crate) fn new(
        default_admin_user_id: impl Into<String>,
        admin_session_ttl_minutes: u64,
        key_session_ttl_minutes: u64,
        store: Arc<dyn AuthStore>,
        verifier: Arc<dyn ClientKeyVerifier>,
    ) -> Self {
        Self {
            default_admin_user_id: default_admin_user_id.into(),
            admin_session_ttl: session_ttl(admin_session_ttl_minutes),
            key_session_ttl: session_ttl(key_session_ttl_minutes),
            store,
            verifier,
        }
    }

    fn auth_audit(&self, action: &str, admin_user_id: &str) -> AdminAuditEvent {
        AdminAuditEvent {
            id: format!("audit_{}", Uuid::now_v7().simple()),
            actor_kind: AuditActorKind::AdminSession,
            actor_admin_user_id: Some(admin_user_id.to_owned()),
            actor_ref: crate::model::auth::admin_session_actor_ref(admin_user_id),
            request_id: None,
            action: action.to_owned(),
            entity_kind: "admin_session".to_owned(),
            entity_ref: admin_user_id.to_owned(),
            config_revision: None,
            changed_fields: Vec::new(),
            occurred_at: Utc::now(),
        }
    }

    async fn authenticate(&self, command: LoginCommand) -> Result<SessionSubject, LoginError> {
        match command {
            LoginCommand::Admin { username, password } => {
                if username.as_deref().unwrap_or(&self.default_admin_user_id)
                    != self.default_admin_user_id
                {
                    return Err(LoginError::InvalidCredentials);
                }
                let password_hash = self
                    .store
                    .load_password_hash(&self.default_admin_user_id)
                    .await
                    .map_err(|_| LoginError::Unavailable)?
                    .ok_or(LoginError::InvalidCredentials)?;
                if password.len() > 4096
                    || !verify_admin_password(&password, &password_hash)
                        .map_err(|_| LoginError::Unavailable)?
                {
                    return Err(LoginError::InvalidCredentials);
                }
                Ok(SessionSubject::Admin {
                    admin_user_id: self.default_admin_user_id.clone(),
                    credential_fingerprint: password_fingerprint(&password_hash),
                })
            }
            LoginCommand::Key { api_key } => {
                if api_key.is_empty() || api_key.len() > 4096 {
                    return Err(LoginError::InvalidCredentials);
                }
                let client_key_id = self.verifier.verify_client_key(&api_key)?;
                if !self
                    .store
                    .client_key_enabled(&client_key_id)
                    .await
                    .map_err(|_| LoginError::Unavailable)?
                {
                    return Err(LoginError::InvalidCredentials);
                }
                Ok(SessionSubject::Key { client_key_id })
            }
        }
    }
}

#[async_trait]
impl AuthService for DefaultAuthService {
    async fn change_password(
        &self,
        session_id: Option<&str>,
        command: ChangePassword,
        source_ip: IpAddr,
    ) -> Result<(), AdminError> {
        let session = self
            .session(session_id)
            .await?
            .ok_or_else(|| AdminError::new(AdminErrorKind::Unauthorized, "请先登录"))?;
        let SessionSubject::Admin {
            admin_user_id,
            credential_fingerprint,
        } = session.subject
        else {
            return Err(AdminError::new(
                AdminErrorKind::Forbidden,
                "仅管理员可以修改密码",
            ));
        };
        if self
            .store
            .consume_login_attempt(
                source_ip,
                LOGIN_ATTEMPTS_PER_SOURCE,
                LOGIN_ATTEMPTS_GLOBAL,
                LOGIN_WINDOW,
            )
            .await
            .map_err(|error| map_store_error(error, "password change limit"))?
            .is_some()
        {
            return Err(AdminError::new(
                AdminErrorKind::RateLimited,
                "尝试过于频繁，请稍后再试",
            ));
        }
        validate_new_password(&command.new_password)?;
        let password_hash = self
            .store
            .load_password_hash(&admin_user_id)
            .await
            .map_err(|error| map_store_error(error, "administrator"))?
            .filter(|hash| password_fingerprint(hash) == credential_fingerprint)
            .ok_or_else(|| AdminError::conflict("密码已变更，请重新登录"))?;
        if command.current_password.len() > 4096
            || !verify_admin_password(&command.current_password, &password_hash)?
        {
            return Err(AdminError::invalid("当前密码不正确"));
        }
        if command.new_password == command.current_password {
            return Err(AdminError::invalid("新密码不能与当前密码相同"));
        }
        let hash = hash_admin_password(&command.new_password)?;
        let mut audit = self.auth_audit("admin.password_changed", &admin_user_id);
        audit.entity_kind = "admin_user".to_owned();
        audit.changed_fields = vec!["password".to_owned()];
        if !self
            .store
            .change_password(&admin_user_id, &password_hash, &hash, audit)
            .await
            .map_err(|error| map_store_error(error, "administrator password"))?
        {
            return Err(AdminError::conflict("密码已变更，请重新登录"));
        }
        // 密码事务提交后旧指纹不再匹配，会话撤销不依赖 Redis 删除成功。
        Ok(())
    }

    async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminError> {
        let hash = hash_admin_password(password)?;
        self.store
            .create_password_hash_if_absent(&self.default_admin_user_id, &hash)
            .await
            .map_err(|error| map_store_error(error, "administrator"))
    }

    async fn session(&self, session_id: Option<&str>) -> Result<Option<AuthSession>, AdminError> {
        let Some(session_id) = session_id.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let Some(session) = self
            .store
            .load_session(session_id)
            .await
            .map_err(|error| map_store_error(error, "authentication session"))?
        else {
            return Ok(None);
        };
        if session.expires_at <= Utc::now() {
            let _ = self.store.delete_session(session_id).await;
            return Ok(None);
        }
        if let SessionSubject::Admin {
            admin_user_id,
            credential_fingerprint,
        } = &session.subject
        {
            let password_hash = self
                .store
                .load_password_hash(admin_user_id)
                .await
                .map_err(|error| map_store_error(error, "administrator session"))?;
            if password_hash
                .is_none_or(|hash| password_fingerprint(&hash) != *credential_fingerprint)
            {
                let _ = self.store.delete_session(session_id).await;
                return Ok(None);
            }
        }
        if let SessionSubject::Key { client_key_id } = &session.subject
            && !self
                .store
                .client_key_enabled(client_key_id)
                .await
                .map_err(|error| map_store_error(error, "session key"))?
        {
            let _ = self.store.delete_session(session_id).await;
            return Ok(None);
        }
        Ok(Some(session))
    }

    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminError> {
        match self
            .session(session_id)
            .await?
            .map(|session| session.subject)
        {
            Some(SessionSubject::Admin { admin_user_id, .. }) => Ok(Some(admin_user_id)),
            Some(SessionSubject::Key { .. }) => Err(AdminError::new(
                AdminErrorKind::Forbidden,
                "当前身份无权访问管理接口",
            )),
            None => Ok(None),
        }
    }

    async fn verify_admin_api_key(&self, key: &str) -> Result<bool, AdminError> {
        if !valid_admin_api_key_shape(key) {
            return Ok(false);
        }
        let stored = self
            .store
            .load_admin_api_key()
            .await
            .map_err(|error| map_store_error(error, "administrator API key"))?;
        Ok(stored.as_ref().is_some_and(|stored| {
            let stored = stored.expose_for_auth();
            key.len() == stored.len() && bool::from(key.as_bytes().ct_eq(stored.as_bytes()))
        }))
    }

    async fn login(
        &self,
        command: LoginCommand,
        source_ip: IpAddr,
        previous_session_id: Option<&str>,
    ) -> Result<LoginResult, LoginError> {
        if let Some(retry_after) = self
            .store
            .consume_login_attempt(
                source_ip,
                LOGIN_ATTEMPTS_PER_SOURCE,
                LOGIN_ATTEMPTS_GLOBAL,
                LOGIN_WINDOW,
            )
            .await
            .map_err(|_| LoginError::Unavailable)?
        {
            return Err(LoginError::TooManyAttempts {
                retry_after_seconds: retry_after.as_secs().max(1),
            });
        }
        let subject = self.authenticate(command).await?;
        let ttl = match subject {
            SessionSubject::Admin { .. } => self.admin_session_ttl,
            SessionSubject::Key { .. } => self.key_session_ttl,
        };
        let session = AuthSession {
            subject,
            expires_at: Utc::now() + ttl,
        };
        let session_id = random_session_token();
        self.store
            .store_session(&session_id, &session)
            .await
            .map_err(|_| LoginError::Unavailable)?;
        if let SessionSubject::Admin { admin_user_id, .. } = &session.subject
            && self
                .store
                .append_audit_event(self.auth_audit("admin.login", admin_user_id))
                .await
                .is_err()
        {
            let _ = self.store.delete_session(&session_id).await;
            return Err(LoginError::Unavailable);
        }
        // 新身份验证成功后才撤销旧会话；撤销失败时不向浏览器提交新会话。
        if let Some(previous) = previous_session_id.filter(|value| !value.is_empty())
            && self.logout(previous).await.is_err()
        {
            let _ = self.store.delete_session(&session_id).await;
            return Err(LoginError::Unavailable);
        }
        Ok(LoginResult {
            session_id,
            session,
        })
    }

    async fn logout(&self, session_id: &str) -> Result<(), AdminError> {
        let session = self
            .store
            .delete_session(session_id)
            .await
            .map_err(|error| map_store_error(error, "authentication session"))?;
        if let Some(AuthSession {
            subject: SessionSubject::Admin { admin_user_id, .. },
            ..
        }) = session
        {
            self.store
                .append_audit_event(self.auth_audit("admin.logout", &admin_user_id))
                .await
                .map_err(|error| map_store_error(error, "administrator audit"))?;
        }
        Ok(())
    }
}

fn session_ttl(minutes: u64) -> Duration {
    Duration::minutes(
        i64::try_from(minutes)
            .unwrap_or(MAX_SESSION_TTL_MINUTES)
            .clamp(1, MAX_SESSION_TTL_MINUTES),
    )
}

fn validate_new_password(password: &str) -> Result<(), AdminError> {
    if password.trim().chars().count() < 12
        || password.len() > 1024
        || password.chars().any(char::is_control)
        || crate::WEAK_ADMIN_PASSWORDS.contains(&password.trim().to_ascii_lowercase().as_str())
    {
        return Err(AdminError::invalid(
            "新密码至少需要 12 个字符，最多 1024 字节，不能使用常见弱口令或控制字符",
        ));
    }
    Ok(())
}

fn hash_admin_password(password: &str) -> Result<String, AdminError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|_| AdminError::internal("管理员密码哈希失败"))
}

fn verify_admin_password(password: &str, encoded: &str) -> Result<bool, AdminError> {
    let hash = PasswordHash::new(encoded)
        .map_err(|_| AdminError::internal("已保存的管理员密码哈希不合法"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_ok())
}

fn random_session_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("session_{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn valid_admin_api_key_shape(value: &str) -> bool {
    value.len() == 70
        && value.starts_with("admin-")
        && value[6..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

// 指纹只绑定已加盐的密码哈希，不把密码或原始哈希复制到 Redis 会话。
fn password_fingerprint(password_hash: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(password_hash.as_bytes()))
}
