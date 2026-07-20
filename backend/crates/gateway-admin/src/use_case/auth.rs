//! 管理员认证状态机。

use std::sync::Arc;

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use rand_core::{OsRng, RngCore as _};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::{
    model::{
        AdminError,
        auth::{
            AdminAuditEvent, AdminSession, AuditActorKind, LoginCommand, LoginError, LoginResult,
        },
    },
    ports::store::AuthStore,
};

use super::map_store_error;

const LOGIN_FAILURE_LIMIT: u32 = 5;
const LOGIN_FAILURE_WINDOW_SECONDS: u64 = 15 * 60;

/// API 鉴权与管理员登录消费的统一服务。
#[async_trait]
pub trait AuthService: Send + Sync {
    async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminError>;
    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminError>;
    async fn verify_admin_api_key(&self, key: &str) -> Result<bool, AdminError>;
    async fn login(&self, command: LoginCommand) -> Result<LoginResult, LoginError>;
    async fn validate_session(&self, session_id: Option<&str>) -> Result<bool, AdminError>;
    async fn logout(&self, session_id: &str) -> Result<(), AdminError>;
}

/// 单管理员认证策略的最终实现。
pub(crate) struct DefaultAuthService {
    default_admin_user_id: String,
    session_ttl: Duration,
    store: Arc<dyn AuthStore>,
}

impl DefaultAuthService {
    #[must_use]
    pub(crate) fn new(
        default_admin_user_id: impl Into<String>,
        session_ttl_minutes: u64,
        store: Arc<dyn AuthStore>,
    ) -> Self {
        let minutes = i64::try_from(session_ttl_minutes)
            .unwrap_or(i64::MAX)
            .max(1);
        Self {
            default_admin_user_id: default_admin_user_id.into(),
            session_ttl: Duration::minutes(minutes),
            store,
        }
    }

    async fn record_failed_login(&self, source: &str) -> Result<LoginError, LoginError> {
        let throttled = self
            .store
            .record_login_failure(source, LOGIN_FAILURE_LIMIT, LOGIN_FAILURE_WINDOW_SECONDS)
            .await
            .map_err(|_| LoginError::Unavailable)?;
        Ok(if throttled {
            LoginError::Throttled
        } else {
            LoginError::InvalidCredentials
        })
    }

    fn auth_audit(&self, action: &str, occurred_at: chrono::DateTime<Utc>) -> AdminAuditEvent {
        AdminAuditEvent {
            id: format!("audit_{}", Uuid::now_v7().simple()),
            actor_kind: AuditActorKind::AdminSession,
            actor_admin_user_id: Some(self.default_admin_user_id.clone()),
            actor_ref: format!("admin:{}", self.default_admin_user_id),
            request_id: None,
            action: action.to_owned(),
            entity_kind: "admin_session".to_owned(),
            entity_ref: self.default_admin_user_id.clone(),
            config_revision: None,
            changed_fields: Vec::new(),
            occurred_at,
        }
    }
}

#[async_trait]
impl AuthService for DefaultAuthService {
    async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminError> {
        let hash = hash_admin_password(password)?;
        self.store
            .create_password_hash_if_absent(&self.default_admin_user_id, &hash)
            .await
            .map_err(|error| map_store_error(error, "administrator"))
    }

    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminError> {
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        self.store
            .load_session(session_id)
            .await
            .map(|session| {
                session
                    .filter(|session| session.expires_at > Utc::now())
                    .map(|session| session.admin_user_id)
            })
            .map_err(|error| map_store_error(error, "administrator session"))
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

    async fn login(&self, command: LoginCommand) -> Result<LoginResult, LoginError> {
        let source = normalized_login_source(&command.source);
        if self
            .store
            .login_source_is_throttled(source, LOGIN_FAILURE_LIMIT, LOGIN_FAILURE_WINDOW_SECONDS)
            .await
            .map_err(|_| LoginError::Unavailable)?
        {
            return Err(LoginError::Throttled);
        }
        if command
            .username
            .as_deref()
            .unwrap_or(&self.default_admin_user_id)
            != self.default_admin_user_id
        {
            return Err(self.record_failed_login(source).await?);
        }
        let hash = self
            .store
            .load_password_hash(&self.default_admin_user_id)
            .await
            .map_err(|_| LoginError::Unavailable)?
            .ok_or(LoginError::InvalidCredentials)?;
        if !verify_admin_password(&command.password, &hash).map_err(|_| LoginError::Unavailable)? {
            return Err(self.record_failed_login(source).await?);
        }

        self.store
            .clear_login_failures(source)
            .await
            .map_err(|_| LoginError::Unavailable)?;
        let session_id = random_session_token();
        let expires_at = Utc::now() + self.session_ttl;
        self.store
            .store_session(
                &session_id,
                &AdminSession {
                    admin_user_id: self.default_admin_user_id.clone(),
                    expires_at,
                },
            )
            .await
            .map_err(|_| LoginError::Unavailable)?;
        if self
            .store
            .append_audit_event(self.auth_audit("admin.login", Utc::now()))
            .await
            .is_err()
        {
            let _ = self.store.delete_session(&session_id).await;
            return Err(LoginError::Unavailable);
        }
        Ok(LoginResult {
            session_id,
            expires_at,
        })
    }

    async fn validate_session(&self, session_id: Option<&str>) -> Result<bool, AdminError> {
        Ok(self.resolve_admin_user_id(session_id).await?.is_some())
    }

    async fn logout(&self, session_id: &str) -> Result<(), AdminError> {
        let session = self
            .store
            .delete_session(session_id)
            .await
            .map_err(|error| map_store_error(error, "administrator session"))?;
        if let Some(session) = session {
            let mut event = self.auth_audit("admin.logout", Utc::now());
            event.actor_admin_user_id = Some(session.admin_user_id.clone());
            event.actor_ref = format!("admin:{}", session.admin_user_id);
            event.entity_ref = session.admin_user_id;
            self.store
                .append_audit_event(event)
                .await
                .map_err(|error| map_store_error(error, "administrator audit"))?;
        }
        Ok(())
    }
}

fn hash_admin_password(password: &str) -> Result<String, AdminError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AdminError::internal("Failed to hash administrator password"))
}

fn verify_admin_password(password: &str, encoded: &str) -> Result<bool, AdminError> {
    let hash = PasswordHash::new(encoded)
        .map_err(|_| AdminError::internal("Stored administrator password hash is invalid"))?;
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

fn normalized_login_source(source: &str) -> &str {
    let source = source.trim();
    if source.is_empty() { "unknown" } else { source }
}
