//! `admin_users` 与 `admin_audit_events` 的唯一 PostgreSQL owner。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{Revision, StoreError, StoreResult, postgres_unavailable, require_nonempty};

const ENTITY: &str = "admin audit event";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAuditActorKind {
    AdminSession,
    AdminApiKey,
    System,
    Anonymous,
}

impl AdminAuditActorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdminSession => "admin_session",
            Self::AdminApiKey => "admin_api_key",
            Self::System => "system",
            Self::Anonymous => "anonymous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAuditEvent {
    pub id: String,
    pub actor_kind: AdminAuditActorKind,
    pub actor_admin_user_id: Option<String>,
    pub actor_ref: String,
    pub admin_request_id: Option<String>,
    pub action: String,
    pub entity_kind: String,
    pub entity_ref: String,
    pub config_revision: Option<i64>,
    pub changed_fields: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl AdminAuditEvent {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "actor_ref", &self.actor_ref)?;
        require_nonempty(ENTITY, "action", &self.action)?;
        require_nonempty(ENTITY, "entity_kind", &self.entity_kind)?;
        require_nonempty(ENTITY, "entity_ref", &self.entity_ref)?;
        if self.config_revision.is_some_and(|revision| revision <= 0) {
            return Err(invalid("config_revision must be positive"));
        }
        if self.changed_fields.len() > 64
            || self
                .changed_fields
                .iter()
                .any(|field| field.trim().is_empty())
        {
            return Err(invalid(
                "changed_fields must contain at most 64 non-empty names",
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait AdminSecurityAuditRepository: Send + Sync {
    async fn password_hash(&self, admin_user_id: &str) -> StoreResult<Option<String>>;

    async fn create_password_hash_if_absent(
        &self,
        admin_user_id: &str,
        password_hash: &str,
    ) -> StoreResult<bool>;

    async fn append_admin_audit_event(&self, event: AdminAuditEvent) -> StoreResult<()>;
}

#[derive(Clone)]
pub struct PgAdminSecurityAuditRepository {
    pool: PgPool,
}

impl PgAdminSecurityAuditRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AdminSecurityAuditRepository for PgAdminSecurityAuditRepository {
    async fn password_hash(&self, admin_user_id: &str) -> StoreResult<Option<String>> {
        require_nonempty("admin user", "id", admin_user_id)?;
        sqlx::query_scalar("select password_hash from admin_users where id = $1")
            .bind(admin_user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("read admin password hash"))
    }

    async fn create_password_hash_if_absent(
        &self,
        admin_user_id: &str,
        password_hash: &str,
    ) -> StoreResult<bool> {
        require_nonempty("admin user", "id", admin_user_id)?;
        require_nonempty("admin user", "password_hash", password_hash)?;
        let result = sqlx::query(
            "insert into admin_users (id, password_hash, created_at, updated_at)
             values ($1, $2, now(), now())
             on conflict (id) do nothing",
        )
        .bind(admin_user_id)
        .bind(password_hash)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("create admin password hash"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn append_admin_audit_event(&self, event: AdminAuditEvent) -> StoreResult<()> {
        event.validate()?;
        sqlx::query(
            "insert into admin_audit_events (
               id, actor_kind, actor_admin_user_id, actor_ref, admin_request_id,
               action, entity_kind, entity_ref, config_revision, changed_fields, created_at
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(event.id)
        .bind(event.actor_kind.as_str())
        .bind(event.actor_admin_user_id)
        .bind(event.actor_ref)
        .bind(event.admin_request_id)
        .bind(event.action)
        .bind(event.entity_kind)
        .bind(event.entity_ref)
        .bind(event.config_revision)
        .bind(event.changed_fields)
        .bind(event.created_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("append admin audit event"))?;
        Ok(())
    }
}

pub(crate) async fn append_admin_audit_event_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    mut event: AdminAuditEvent,
    revision: Revision,
) -> StoreResult<()> {
    event.config_revision = Some(
        i64::try_from(revision.get())
            .map_err(|_| invalid("config revision exceeds PostgreSQL bigint"))?,
    );
    event.validate()?;
    sqlx::query(
        "insert into admin_audit_events (
           id, actor_kind, actor_admin_user_id, actor_ref, admin_request_id,
           action, entity_kind, entity_ref, config_revision, changed_fields, created_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(event.id)
    .bind(event.actor_kind.as_str())
    .bind(event.actor_admin_user_id)
    .bind(event.actor_ref)
    .bind(event.admin_request_id)
    .bind(event.action)
    .bind(event.entity_kind)
    .bind(event.entity_ref)
    .bind(event.config_revision)
    .bind(event.changed_fields)
    .bind(event.created_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("append admin audit event in transaction"))?;
    Ok(())
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: message.to_owned(),
    }
}
