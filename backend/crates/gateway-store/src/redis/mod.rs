//! 可丢失、可从 PostgreSQL 或 Provider 重建的 Redis 协调状态。

use chrono::{DateTime, SecondsFormat, Utc};
use redis::{Script, aio::ConnectionManager};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use async_trait::async_trait;

mod client_admission;
mod credential_cooldown;
mod credential_leases;
mod credential_state;
mod provider_circuit;
mod runtime_change;

pub use client_admission::*;
pub use credential_cooldown::*;
pub use credential_leases::*;
pub use credential_state::*;
pub use provider_circuit::*;
pub use runtime_change::*;

use crate::{StoreError, StoreResult, redis_unavailable, require_nonempty};

pub(crate) const MAX_REDIS_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;

const RECORD_ADMIN_LOGIN_FAILURE_SCRIPT: &str = r#"
local count = redis.call('INCR', KEYS[1])
local ttl = redis.call('PTTL', KEYS[1])
if count == 1 or ttl < 0 then
  redis.call('PEXPIRE', KEYS[1], ARGV[2])
end
if count >= tonumber(ARGV[1]) then
  return 1
end
return 0
"#;

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

/// 管理员会话与登录失败窗口的 Redis 基础设施端口。
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
    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> StoreResult<bool>;
    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> StoreResult<bool>;
    async fn clear_login_failures(&self, source: &str) -> StoreResult<()>;
}

/// Redis 管理员会话与登录失败窗口 adapter。
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

    fn failure_key(&self, source: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("admin login failure", source)?;
        Ok(format!("{}:failure:{{{fingerprint}}}", self.namespace))
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

    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> StoreResult<bool> {
        validate_failure_policy(failure_limit, window_seconds)?;
        let key = self.failure_key(source)?;
        let mut connection = self.connection.clone();
        let failures = redis::cmd("GET")
            .arg(key)
            .query_async::<Option<u64>>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("load admin login failure window"))?
            .unwrap_or_default();
        Ok(failures >= u64::from(failure_limit))
    }

    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> StoreResult<bool> {
        let window_millis = validate_failure_policy(failure_limit, window_seconds)?;
        let key = self.failure_key(source)?;
        let mut connection = self.connection.clone();
        let throttled = Script::new(RECORD_ADMIN_LOGIN_FAILURE_SCRIPT)
            .key(key)
            .arg(failure_limit)
            .arg(window_millis)
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("record admin login failure"))?;
        Ok(throttled == 1)
    }

    async fn clear_login_failures(&self, source: &str) -> StoreResult<()> {
        let key = self.failure_key(source)?;
        let mut connection = self.connection.clone();
        redis::cmd("DEL")
            .arg(key)
            .query_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("clear admin login failures"))?;
        Ok(())
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

fn validate_failure_policy(failure_limit: u32, window_seconds: u64) -> StoreResult<u64> {
    if failure_limit == 0 {
        return Err(admin_auth_invalid("login failure limit must be positive"));
    }
    let window_millis = window_seconds
        .checked_mul(1_000)
        .filter(|value| *value > 0 && *value <= MAX_REDIS_EXACT_INTEGER)
        .ok_or_else(|| admin_auth_invalid("login failure window is outside the supported range"))?;
    Ok(window_millis)
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
