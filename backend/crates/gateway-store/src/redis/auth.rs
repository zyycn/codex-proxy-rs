//! 控制面统一会话与双层固定窗口登录限流。

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use redis::{Script, aio::ConnectionManager};
use serde::{Deserialize, Serialize};

use super::{MAX_REDIS_EXACT_INTEGER, namespace, resource_fingerprint};
use crate::{StoreError, StoreResult, redis_unavailable, require_nonempty};

const CONSUME_LOGIN_ATTEMPT_SCRIPT: &str = r#"
local source_count = redis.call('INCR', KEYS[1])
if source_count == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[3]) end
local global_count = redis.call('INCR', KEYS[2])
if global_count == 1 then redis.call('PEXPIRE', KEYS[2], ARGV[3]) end
if source_count > tonumber(ARGV[1]) or global_count > tonumber(ARGV[2]) then
  return math.max(redis.call('PTTL', KEYS[1]), redis.call('PTTL', KEYS[2]), 1)
end
return 0
"#;

/// Redis 身份标签只由认证服务写入，不能从请求中的角色声明构造。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionSubjectRecord {
    Admin {
        admin_user_id: String,
        // 旧会话没有指纹，按未认证处理并要求重新登录。
        #[serde(default)]
        credential_fingerprint: String,
    },
    Key {
        client_key_id: String,
    },
}

impl SessionSubjectRecord {
    fn validate(&self) -> StoreResult<()> {
        match self {
            Self::Admin { admin_user_id, .. } => {
                require_nonempty("authentication session", "admin_user_id", admin_user_id)
            }
            Self::Key { client_key_id } => {
                require_nonempty("authentication session", "client_key_id", client_key_id)
            }
        }
    }
}

/// Redis 中不含密码或原始 API Key 的统一会话事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSessionRecord {
    pub subject: SessionSubjectRecord,
    pub expires_at: DateTime<Utc>,
}

impl AuthSessionRecord {
    fn validate(&self) -> StoreResult<u64> {
        self.subject.validate()?;
        let expires_at_millis = u64::try_from(self.expires_at.timestamp_millis())
            .map_err(|_| auth_invalid("session expiry must be after the Unix epoch"))?;
        if expires_at_millis > MAX_REDIS_EXACT_INTEGER {
            return Err(auth_invalid(
                "session expiry is outside the supported range",
            ));
        }
        let now_millis = u64::try_from(Utc::now().timestamp_millis())
            .map_err(|_| auth_invalid("current time is outside the supported range"))?;
        if expires_at_millis <= now_millis {
            return Err(auth_invalid("session expiry must be in the future"));
        }
        Ok(expires_at_millis)
    }
}

#[async_trait]
pub trait AuthStateRepository: Send + Sync {
    async fn load_session(&self, session_id: &str) -> StoreResult<Option<AuthSessionRecord>>;
    async fn store_session(&self, session_id: &str, session: &AuthSessionRecord)
    -> StoreResult<()>;
    async fn delete_session(&self, session_id: &str) -> StoreResult<Option<AuthSessionRecord>>;
    async fn consume_login_attempt(
        &self,
        source: &str,
        source_limit: u32,
        global_limit: u32,
        window: Duration,
    ) -> StoreResult<Option<Duration>>;
}

#[derive(Clone)]
pub struct RedisAuthStateRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisAuthStateRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: format!("{}:auth:v1", namespace(key_namespace)?),
        })
    }

    fn session_key(&self, session_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("authentication session", session_id)?;
        Ok(format!("{}:session:{{{fingerprint}}}", self.namespace))
    }

    fn login_keys(&self, source: &str) -> StoreResult<(String, String)> {
        let fingerprint = resource_fingerprint("login source", source)?;
        Ok((
            format!("{}:login:{{login}}:source:{fingerprint}", self.namespace),
            format!("{}:login:{{login}}:global", self.namespace),
        ))
    }
}

#[async_trait]
impl AuthStateRepository for RedisAuthStateRepository {
    async fn load_session(&self, session_id: &str) -> StoreResult<Option<AuthSessionRecord>> {
        let key = self.session_key(session_id)?;
        let mut connection = self.connection.clone();
        let payload = redis::cmd("GET")
            .arg(key)
            .query_async::<Option<String>>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("load authentication session"))?;
        payload.map(|value| decode_session(&value)).transpose()
    }

    async fn store_session(
        &self,
        session_id: &str,
        session: &AuthSessionRecord,
    ) -> StoreResult<()> {
        let key = self.session_key(session_id)?;
        let expires_at_millis = session.validate()?;
        let payload = encode_session(session)?;
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg(key)
            .arg(payload)
            .arg("PXAT")
            .arg(expires_at_millis)
            .query_async::<String>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("store authentication session"))?;
        Ok(())
    }

    async fn delete_session(&self, session_id: &str) -> StoreResult<Option<AuthSessionRecord>> {
        let key = self.session_key(session_id)?;
        let mut connection = self.connection.clone();
        let payload = redis::cmd("GETDEL")
            .arg(key)
            .query_async::<Option<String>>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("delete authentication session"))?;
        payload.map(|value| decode_session(&value)).transpose()
    }

    async fn consume_login_attempt(
        &self,
        source: &str,
        source_limit: u32,
        global_limit: u32,
        window: Duration,
    ) -> StoreResult<Option<Duration>> {
        if source_limit == 0 || global_limit == 0 || window.is_zero() {
            return Err(auth_invalid("login rate limit policy is invalid"));
        }
        let window_millis = u64::try_from(window.as_millis())
            .ok()
            .filter(|value| *value > 0 && *value <= MAX_REDIS_EXACT_INTEGER)
            .ok_or_else(|| auth_invalid("login rate limit window is invalid"))?;
        let (source_key, global_key) = self.login_keys(source)?;
        let mut connection = self.connection.clone();
        let retry_after_millis: i64 = Script::new(CONSUME_LOGIN_ATTEMPT_SCRIPT)
            .key(source_key)
            .key(global_key)
            .arg(source_limit)
            .arg(global_limit)
            .arg(window_millis)
            .invoke_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("rate limit authentication login"))?;
        if retry_after_millis <= 0 {
            Ok(None)
        } else {
            Ok(Some(Duration::from_millis(
                u64::try_from(retry_after_millis).unwrap_or(1),
            )))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthSessionWire {
    subject: SessionSubjectRecord,
    expires_at: String,
}

fn encode_session(session: &AuthSessionRecord) -> StoreResult<String> {
    serde_json::to_string(&AuthSessionWire {
        subject: session.subject.clone(),
        expires_at: session
            .expires_at
            .to_rfc3339_opts(SecondsFormat::Nanos, true),
    })
    .map_err(|_| auth_invalid("session value cannot be encoded"))
}

fn decode_session(value: &str) -> StoreResult<AuthSessionRecord> {
    let wire: AuthSessionWire = serde_json::from_str(value)
        .map_err(|_| auth_invalid("Redis returned an invalid session value"))?;
    wire.subject.validate()?;
    let parse = |value: &str| {
        DateTime::parse_from_rfc3339(value)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|_| auth_invalid("Redis returned an invalid session timestamp"))
    };
    let expires_at = parse(&wire.expires_at)?;
    Ok(AuthSessionRecord {
        subject: wire.subject,
        expires_at,
    })
}

fn auth_invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "authentication state",
        message: message.to_owned(),
    }
}
