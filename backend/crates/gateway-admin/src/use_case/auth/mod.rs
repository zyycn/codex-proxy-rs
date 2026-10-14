//! 控制面认证端口、会话策略与访问和续期共用的身份校验

use std::{net::IpAddr, sync::Arc};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use gateway_core::engine::execution::ClientKeyVerifier;
use sha2::{Digest as _, Sha256};

use super::map_store_error;
use crate::{
    model::{
        AdminError,
        auth::{
            AuthSession, ChangePassword, LoginCommand, LoginError, SessionGrant, SessionSubject,
        },
    },
    ports::store::AuthStore,
};

mod service;
mod session;

/// 所有控制面接口消费同一个会话服务，权限由服务端身份决定
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
    async fn renew_session(
        &self,
        refresh_token: Option<&str>,
        access_token: Option<&str>,
    ) -> Result<Option<SessionGrant>, AdminError>;
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
    ) -> Result<SessionGrant, LoginError>;
    async fn logout(&self, session_id: &str) -> Result<(), AdminError>;
}

const MAX_SESSION_TTL_MINUTES: i64 = 366 * 24 * 60;

pub(crate) struct DefaultAuthService {
    default_admin_user_id: String,
    admin_access_ttl: Duration,
    admin_session_ttl: Duration,
    admin_session_absolute_ttl: Duration,
    key_session_ttl: Duration,
    store: Arc<dyn AuthStore>,
    verifier: Arc<dyn ClientKeyVerifier>,
}

impl DefaultAuthService {
    #[must_use]
    pub(crate) fn new(
        config: &crate::AdminConfig,
        key_session_ttl_minutes: u64,
        store: Arc<dyn AuthStore>,
        verifier: Arc<dyn ClientKeyVerifier>,
    ) -> Self {
        let admin_session_ttl = session_ttl(config.session_ttl_minutes);
        Self {
            default_admin_user_id: config.default_username.clone(),
            // 访问凭据最多有效 24 小时，较短的不活动期限也要留出续期窗口
            admin_access_ttl: Duration::hours(24).min(admin_session_ttl / 4),
            admin_session_ttl,
            admin_session_absolute_ttl: session_ttl(config.session_absolute_ttl_minutes),
            key_session_ttl: session_ttl(key_session_ttl_minutes),
            store,
            verifier,
        }
    }

    async fn load_live_session(&self, id: &str) -> Result<Option<AuthSession>, AdminError> {
        let Some(session) = self
            .store
            .load_session(id)
            .await
            .map_err(|error| map_store_error(error, "authentication session"))?
        else {
            return Ok(None);
        };
        if session.retained_until() <= Utc::now()
            || session
                .absolute_expires_at
                .is_some_and(|expiry| expiry <= Utc::now())
        {
            return Ok(None);
        }
        let valid = match &session.subject {
            SessionSubject::Admin {
                admin_user_id,
                credential_fingerprint,
            } => self
                .store
                .load_password_hash(admin_user_id)
                .await
                .map_err(|error| map_store_error(error, "administrator session"))?
                .is_some_and(|hash| password_fingerprint(&hash) == *credential_fingerprint),
            SessionSubject::Key { client_key_id } => self
                .store
                .client_key_enabled(client_key_id)
                .await
                .map_err(|error| map_store_error(error, "session key"))?,
        };
        if !valid {
            let _ = self.store.delete_session(id).await;
            return Ok(None);
        }
        Ok(Some(session))
    }
}

fn session_ttl(minutes: u64) -> Duration {
    Duration::minutes(
        i64::try_from(minutes)
            .unwrap_or(MAX_SESSION_TTL_MINUTES)
            .clamp(1, MAX_SESSION_TTL_MINUTES),
    )
}

// 指纹只绑定已加盐的密码哈希，不把密码或原始哈希复制到 Redis 会话
fn password_fingerprint(password_hash: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(password_hash.as_bytes()))
}
