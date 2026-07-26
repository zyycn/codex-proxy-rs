//! 可丢失、可从 PostgreSQL 或 Provider 重建的 Redis 协调状态。

use chrono::{DateTime, SecondsFormat, Utc};
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use async_trait::async_trait;

mod client_admission;
mod credential_cooldown;
mod credential_leases;
mod credential_state;
mod native_continuation;
mod oauth_pending;
mod provider_circuit;
mod provider_session_affinity;
mod provider_session_exclusion;
mod runtime_change;
pub(crate) mod worker_lease;

pub use client_admission::*;
pub use credential_cooldown::*;
pub use credential_leases::*;
pub use credential_state::*;
pub use native_continuation::*;
pub use oauth_pending::*;
pub use provider_circuit::*;
pub use provider_session_affinity::*;
pub use provider_session_exclusion::*;
pub use runtime_change::*;

use crate::{StoreError, StoreResult, redis_unavailable, require_nonempty};

pub(crate) const MAX_REDIS_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;

/// Redis 中可丢失的管理员会话事实；认证秘密不属于该结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSessionRecord {
    pub admin_user_id: String,
    pub expires_at: DateTime<Utc>,
}

impl AdminSessionRecord {
    fn validate(&self) -> StoreResult<u64> {
        require_nonempty("admin session", "admin_user_id", &self.admin_user_id)?;
        let expires_at_millis = u64::try_from(self.expires_at.timestamp_millis())
            .map_err(|_| admin_auth_invalid("session expiry must be after the Unix epoch"))?;
        if expires_at_millis > MAX_REDIS_EXACT_INTEGER {
            return Err(admin_auth_invalid(
                "session expiry is outside the supported range",
            ));
        }
        let now_millis = u64::try_from(Utc::now().timestamp_millis())
            .map_err(|_| admin_auth_invalid("current time is outside the supported range"))?;
        if expires_at_millis <= now_millis {
            return Err(admin_auth_invalid("session expiry must be in the future"));
        }
        Ok(expires_at_millis)
    }
}

/// 管理员会话的 Redis 基础设施端口。
#[async_trait]
pub trait AdminAuthStateRepository: Send + Sync {
    async fn load_admin_session(&self, session_id: &str)
    -> StoreResult<Option<AdminSessionRecord>>;
    async fn store_admin_session(
        &self,
        session_id: &str,
        session: &AdminSessionRecord,
    ) -> StoreResult<()>;
    async fn delete_admin_session(
        &self,
        session_id: &str,
    ) -> StoreResult<Option<AdminSessionRecord>>;
}

/// Redis 管理员会话 adapter。
#[derive(Clone)]
pub struct RedisAdminAuthStateRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisAdminAuthStateRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: format!("{}:admin-auth:v1", namespace(key_namespace)?),
        })
    }

    fn session_key(&self, session_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("admin session", session_id)?;
        Ok(format!("{}:session:{{{fingerprint}}}", self.namespace))
    }
}

#[async_trait]
impl AdminAuthStateRepository for RedisAdminAuthStateRepository {
    async fn load_admin_session(
        &self,
        session_id: &str,
    ) -> StoreResult<Option<AdminSessionRecord>> {
        let key = self.session_key(session_id)?;
        let mut connection = self.connection.clone();
        let payload = redis::cmd("GET")
            .arg(key)
            .query_async::<Option<String>>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("load admin session"))?;
        payload
            .map(|value| decode_admin_session(&value))
            .transpose()
    }

    async fn store_admin_session(
        &self,
        session_id: &str,
        session: &AdminSessionRecord,
    ) -> StoreResult<()> {
        let key = self.session_key(session_id)?;
        let expires_at_millis = session.validate()?;
        let payload = encode_admin_session(session)?;
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg(key)
            .arg(payload)
            .arg("PXAT")
            .arg(expires_at_millis)
            .query_async::<String>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("store admin session"))?;
        Ok(())
    }

    async fn delete_admin_session(
        &self,
        session_id: &str,
    ) -> StoreResult<Option<AdminSessionRecord>> {
        let key = self.session_key(session_id)?;
        let mut connection = self.connection.clone();
        let payload = redis::cmd("GETDEL")
            .arg(key)
            .query_async::<Option<String>>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("delete admin session"))?;
        payload
            .map(|value| decode_admin_session(&value))
            .transpose()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminSessionWire {
    admin_user_id: String,
    expires_at: String,
}

fn encode_admin_session(session: &AdminSessionRecord) -> StoreResult<String> {
    serde_json::to_string(&AdminSessionWire {
        admin_user_id: session.admin_user_id.clone(),
        expires_at: session
            .expires_at
            .to_rfc3339_opts(SecondsFormat::Nanos, true),
    })
    .map_err(|_| admin_auth_invalid("session value cannot be encoded"))
}

fn decode_admin_session(value: &str) -> StoreResult<AdminSessionRecord> {
    let wire: AdminSessionWire = serde_json::from_str(value)
        .map_err(|_| admin_auth_invalid("Redis returned an invalid session value"))?;
    require_nonempty("admin session", "admin_user_id", &wire.admin_user_id)?;
    let expires_at = DateTime::parse_from_rfc3339(&wire.expires_at)
        .map_err(|_| admin_auth_invalid("Redis returned an invalid session expiry"))?
        .with_timezone(&Utc);
    Ok(AdminSessionRecord {
        admin_user_id: wire.admin_user_id,
        expires_at,
    })
}

fn admin_auth_invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "admin authentication state",
        message: message.to_owned(),
    }
}

pub(crate) fn resource_fingerprint(entity: &'static str, value: &str) -> StoreResult<String> {
    require_nonempty(entity, "resource ID", value)?;
    Ok(hex::encode(Sha256::digest(value.as_bytes())))
}

pub(crate) fn namespace(value: &str) -> StoreResult<String> {
    require_nonempty("Redis namespace", "namespace", value)?;
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(StoreError::InvalidData {
            entity: "Redis namespace",
            message: "namespace contains unsupported characters".to_owned(),
        });
    }
    Ok(value.to_owned())
}
