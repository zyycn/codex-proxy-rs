//! 管理员认证、会话和安全审计事实。

use chrono::{DateTime, Utc};

use super::{MutationActor, MutationContext, Revision};

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

/// 管理员登录命令。
#[derive(Clone, PartialEq, Eq)]
pub struct LoginCommand {
    pub username: Option<String>,
    pub password: String,
    pub source: String,
}

impl std::fmt::Debug for LoginCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginCommand")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("source", &self.source)
            .finish()
    }
}

/// 登录成功后返回的会话事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginResult {
    pub session_id: String,
    pub expires_at: DateTime<Utc>,
}

/// 登录状态机可被 API 精确映射的失败类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    #[error("invalid administrator credentials")]
    InvalidCredentials,
    #[error("administrator login is throttled")]
    Throttled,
    #[error("administrator authentication is unavailable")]
    Unavailable,
}

/// Redis 中可恢复的管理员会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSession {
    pub admin_user_id: String,
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
