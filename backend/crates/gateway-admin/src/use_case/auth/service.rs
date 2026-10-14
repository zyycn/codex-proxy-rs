//! 管理员和 Key 的登录、退出、密码策略与会话访问

use std::{net::IpAddr, time::Duration as StdDuration};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use rand_core::{OsRng, RngCore as _};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use super::{
    AuthService, DefaultAuthService, password_fingerprint,
    session::{self, Credential, storage_id},
};
use crate::{
    model::{
        AdminError, AdminErrorKind,
        auth::{
            AdminAuditEvent, AuditActorKind, AuthSession, ChangePassword, LoginCommand, LoginError,
            SessionGrant, SessionSubject,
        },
    },
    use_case::map_store_error,
};

const LOGIN_WINDOW: StdDuration = StdDuration::from_secs(60);
const LOGIN_ATTEMPTS_PER_SOURCE: u32 = 10;
const LOGIN_ATTEMPTS_GLOBAL: u32 = 200;

impl DefaultAuthService {
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
        // 密码事务提交后旧指纹不再匹配，会话撤销不依赖 Redis 删除成功
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
        let Some(raw) = session_id.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let Some(session) = self.load_live_session(storage_id(raw)).await? else {
            return Ok(None);
        };
        if session.expires_at <= Utc::now() {
            return Ok(None);
        }
        if let Some(renewal) = &session.renewal {
            let Some(credential) = Credential::parse(raw).filter(|value| !value.refresh) else {
                return Ok(None);
            };
            if !credential.owns(renewal) {
                return Ok(None);
            }
        } else if Credential::parse(raw).is_some() {
            return Ok(None);
        }
        Ok(Some(session))
    }

    async fn renew_session(
        &self,
        refresh_token: Option<&str>,
        access_token: Option<&str>,
    ) -> Result<Option<SessionGrant>, AdminError> {
        if let Some(refresh) = refresh_token {
            return self.refresh(refresh).await;
        }
        // Key 与升级前的单凭据仅返回仍有效的原会话，不授予新的续期能力
        let Some(session) = self
            .session(access_token)
            .await?
            .filter(|session| session.renewal.is_none())
        else {
            return Ok(None);
        };
        Ok(Some(SessionGrant {
            session_id: access_token.unwrap_or_default().to_owned(),
            refresh_token: None,
            session,
        }))
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
    ) -> Result<SessionGrant, LoginError> {
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
        let now = Utc::now();
        let absolute_expires_at = matches!(subject, SessionSubject::Admin { .. })
            .then_some(now + self.admin_session_absolute_ttl);
        let refresh_expiry = (now + ttl).min(absolute_expires_at.unwrap_or(now + ttl));
        let grant = if let Some(absolute) = absolute_expires_at {
            session::issue(
                subject,
                (now + self.admin_access_ttl).min(refresh_expiry),
                refresh_expiry,
                absolute,
            )
            .map_err(|_| LoginError::Unavailable)?
        } else {
            SessionGrant {
                session_id: random_session_token(),
                refresh_token: None,
                session: AuthSession {
                    subject,
                    expires_at: refresh_expiry,
                    absolute_expires_at: None,
                    renewal: None,
                },
            }
        };
        let session_id = storage_id(&grant.session_id);
        let session = &grant.session;
        self.store
            .store_session(session_id, session)
            .await
            .map_err(|_| LoginError::Unavailable)?;
        if let SessionSubject::Admin { admin_user_id, .. } = &session.subject
            && self
                .store
                .append_audit_event(self.auth_audit("admin.login", admin_user_id))
                .await
                .is_err()
        {
            let _ = self.store.delete_session(session_id).await;
            return Err(LoginError::Unavailable);
        }
        // 新身份验证成功后才撤销旧会话；撤销失败时不向浏览器提交新会话
        if let Some(previous) = previous_session_id.filter(|value| !value.is_empty())
            && self.logout(previous).await.is_err()
        {
            let _ = self.store.delete_session(session_id).await;
            return Err(LoginError::Unavailable);
        }
        Ok(grant)
    }

    async fn logout(&self, session_id: &str) -> Result<(), AdminError> {
        let id = storage_id(session_id);
        let Some(stored) = self
            .store
            .load_session(id)
            .await
            .map_err(|error| map_store_error(error, "authentication session"))?
        else {
            return Ok(());
        };
        if let Some(renewal) = &stored.renewal {
            if !Credential::parse(session_id).is_some_and(|credential| credential.owns(renewal)) {
                return Ok(());
            }
        } else if Credential::parse(session_id).is_some() {
            return Ok(());
        }
        let session = self
            .store
            .delete_session(id)
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
