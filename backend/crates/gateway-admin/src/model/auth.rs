//! 控制面认证、统一会话和管理员安全审计事实。

use chrono::{DateTime, Utc};
use gateway_core::{engine::execution::ClientAuthenticationError, policy::ClientApiKeyId};

use super::{MutationActor, MutationContext, Revision};

/// 会话审计使用的稳定管理员标识。
#[must_use]
pub fn admin_session_actor_ref(admin_user_id: &str) -> String {
    format!("admin:{admin_user_id}")
}

/// 已认证的管理主体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminPrincipal {
    Session { admin_user_id: String },
    ApiKey,
}

/// 传给管理用例的安全请求上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminRequestContext {
    pub principal: AdminPrincipal,
    pub request_id: String,
}

impl AdminRequestContext {
    #[must_use]
    pub fn mutation_context(&self) -> MutationContext {
        let actor = match &self.principal {
            AdminPrincipal::Session { admin_user_id } => MutationActor::AdminSession {
                admin_user_id: admin_user_id.clone(),
            },
            AdminPrincipal::ApiKey => MutationActor::AdminApiKey,
        };
        MutationContext {
            actor,
            request_id: self.request_id.clone(),
        }
    }
}

/// 登录方式只决定凭据验证流程，不能直接授予会话权限。
#[derive(Clone, PartialEq, Eq)]
pub enum LoginCommand {
    Admin {
        username: Option<String>,
        password: String,
    },
    Key {
        api_key: String,
    },
}

impl std::fmt::Debug for LoginCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admin { username, .. } => formatter
                .debug_struct("AdminLogin")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::Key { .. } => formatter
                .debug_struct("KeyLogin")
                .field("api_key", &"[REDACTED]")
                .finish(),
        }
    }
}

/// 登录成功后返回的会话事实。
#[derive(Clone, PartialEq, Eq)]
pub struct LoginResult {
    pub session_id: String,
    pub session: AuthSession,
}

impl std::fmt::Debug for LoginResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginResult")
            .field("session_id", &"[REDACTED]")
            .field("session", &self.session)
            .finish()
    }
}

/// 登录状态机可被 API 精确映射的失败类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    #[error("invalid login credentials")]
    InvalidCredentials,
    #[error("too many login attempts")]
    TooManyAttempts { retry_after_seconds: u64 },
    #[error("authentication is unavailable")]
    Unavailable,
}

impl From<ClientAuthenticationError> for LoginError {
    fn from(error: ClientAuthenticationError) -> Self {
        match error {
            ClientAuthenticationError::InvalidKey => Self::InvalidCredentials,
            ClientAuthenticationError::SnapshotUnavailable => Self::Unavailable,
        }
    }
}

/// 服务端已验证的身份绑定，不保存密码或原始 API Key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSubject {
    Admin {
        admin_user_id: String,
        credential_fingerprint: String,
    },
    Key {
        client_key_id: ClientApiKeyId,
    },
}

/// 仅已登录管理员可以修改自己的密码。
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

impl std::fmt::Debug for ChangePassword {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ChangePassword([REDACTED])")
    }
}

/// 两种登录方式共用的固定有效期会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSession {
    pub subject: SessionSubject,
    pub expires_at: DateTime<Utc>,
}

/// 安全审计事件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditActorKind {
    AdminSession,
    AdminApiKey,
    System,
    Anonymous,
}

/// 管理写操作留下的最小审计事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAuditEvent {
    pub id: String,
    pub actor_kind: AuditActorKind,
    pub actor_admin_user_id: Option<String>,
    pub actor_ref: String,
    pub request_id: Option<String>,
    pub action: String,
    pub entity_kind: String,
    pub entity_ref: String,
    pub config_revision: Option<Revision>,
    pub changed_fields: Vec<String>,
    pub occurred_at: DateTime<Utc>,
}
