//! 会话账号绑定的 Redis 版本快照、原子认领与续期

use std::time::Duration;

use gateway_core::account::ProviderAccountId;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    ProviderAccountAdmission, ProviderSessionAffinityKey, ProviderSessionAffinityPort,
    ProviderSessionBinding, ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use redis::aio::ConnectionManager;

use crate::StoreResult;

use super::{namespace, resource_fingerprint};

const MAX_SESSION_AFFINITY_TTL: Duration =
    Duration::from_secs(gateway_core::account::MAX_SESSION_AFFINITY_TTL_HOURS as u64 * 3600);

const CURSOR_TTL_MILLIS: u64 = 24 * 60 * 60 * 1000;

// 先比较全部快照再写入；失去首绑或轮询位置的请求不能续期或消耗名额
const ADMIT_ACCOUNT_SCRIPT: &str = r#"
if ARGV[2] ~= '' and (redis.call('GET', KEYS[1]) or '') ~= ARGV[1] then return 0 end
if ARGV[4] ~= '' and (redis.call('GET', KEYS[#KEYS]) or '0') ~= ARGV[4] then return 0 end
if ARGV[2] ~= '' then redis.call('PSETEX', KEYS[1], ARGV[3], ARGV[2]) end
if ARGV[4] ~= '' then redis.call('PSETEX', KEYS[#KEYS], ARGV[6], ARGV[5]) end
return 1
"#;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindingRecord {
    account_id: String,
    revision: String,
}

impl BindingRecord {
    fn encode(binding: &ProviderSessionBinding) -> Result<String, ProviderStoreError> {
        serde_json::to_string(&Self {
            account_id: binding.account_id().as_str().to_owned(),
            revision: binding.revision().to_owned(),
        })
        .map_err(|_| provider_invalid("encode provider session binding"))
    }

    fn decode(raw: &str) -> Result<ProviderSessionBinding, ProviderStoreError> {
        let record: Self = serde_json::from_str(raw)
            .map_err(|_| provider_invalid("decode provider session binding"))?;
        let account = ProviderAccountId::new(record.account_id)
            .map_err(|_| provider_invalid("decode provider session binding account"))?;
        ProviderSessionBinding::new(account, record.revision)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AliasRecord {
    session_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_session_key: Option<String>,
    follow_only: bool,
}

#[derive(Clone)]
pub struct RedisProviderSessionAffinityRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisProviderSessionAffinityRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn cursor_key(
        &self,
        provider: &ProviderKind,
        client_key: &ClientApiKeyId,
    ) -> Result<String, ProviderStoreError> {
        super::scheduling_cursor_key(&self.namespace, client_key, provider, "session-cursor")
            .map_err(|_| provider_invalid("encode provider round robin cursor key"))
    }

    fn key(
        &self,
        provider_kind: &ProviderKind,
        affinity_key: &ProviderSessionAffinityKey,
    ) -> Result<String, ProviderStoreError> {
        let scope = format!(
            "{}\0{}",
            provider_kind.as_str(),
            affinity_key.expose_to_store()
        );
        let fingerprint = resource_fingerprint("provider session affinity", &scope)
            .map_err(|_| provider_invalid("encode provider session affinity key"))?;
        Ok(format!(
            "{}:scheduler:session-binding:{{{fingerprint}}}",
            self.namespace
        ))
    }
}

impl ProviderSessionAffinityPort for RedisProviderSessionAffinityRepository {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderSessionBinding>, ProviderStoreError>>
    {
        Box::pin(async move {
            let raw = redis::cmd("GET")
                .arg(self.key(provider_kind, key)?)
                .query_async::<Option<String>>(&mut self.connection.clone())
                .await
                .map_err(|source| {
                    crate::provider_unavailable("load provider session binding", source)
                })?;
            raw.as_deref().map(BindingRecord::decode).transpose()
        })
    }

    fn round_robin_cursor<'a>(
        &'a self,
        provider: &'a ProviderKind,
        client_key: &'a ClientApiKeyId,
    ) -> futures::future::BoxFuture<'a, Result<u64, ProviderStoreError>> {
        Box::pin(async move {
            let key = self.cursor_key(provider, client_key)?;
            redis::cmd("GET")
                .arg(key)
                .query_async::<Option<u64>>(&mut self.connection.clone())
                .await
                .map(Option::unwrap_or_default)
                .map_err(|source| crate::provider_unavailable("read round robin cursor", source))
        })
    }

    fn admit_account<'a>(
        &'a self,
        provider: &'a ProviderKind,
        admission: ProviderAccountAdmission<'a>,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let script = redis::Script::new(ADMIT_ACCOUNT_SCRIPT);
            let mut invocation = script.prepare_invoke();
            if let Some(claim) = admission.session() {
                let binding = match claim.expected.filter(|b| b.account_id() == claim.account) {
                    Some(existing) => existing.clone(),
                    None => ProviderSessionBinding::new(
                        claim.account.clone(),
                        uuid::Uuid::new_v4().simple().to_string(),
                    )?,
                };
                invocation
                    .key(self.key(provider, claim.key)?)
                    .arg(
                        claim
                            .expected
                            .map(BindingRecord::encode)
                            .transpose()?
                            .unwrap_or_default(),
                    )
                    .arg(BindingRecord::encode(&binding)?)
                    .arg(session_affinity_ttl_millis(claim.ttl)?);
            } else {
                invocation.arg("").arg("").arg(0);
            }
            if let ProviderAccountAdmission::RoundRobin {
                client_key, cursor, ..
            } = admission
            {
                invocation
                    .key(self.cursor_key(provider, client_key)?)
                    .arg(cursor)
                    .arg(cursor.wrapping_add(1))
                    .arg(CURSOR_TTL_MILLIS);
            } else {
                invocation.arg("");
            }
            invocation
                .invoke_async(&mut self.connection.clone())
                .await
                .map_err(|source| crate::provider_unavailable("admit provider account", source))
        })
    }

    fn load_alias<'a>(
        &'a self,
        provider: &'a ProviderKind,
        alias: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Option<gateway_core::provider_ports::ProviderSessionAlias>, ProviderStoreError>,
    > {
        Box::pin(async move {
            let value = redis::cmd("GET")
                .arg(format!("{}:alias", self.key(provider, alias)?))
                .query_async::<Option<String>>(&mut self.connection.clone())
                .await
                .map_err(|source| crate::provider_unavailable("load session alias", source))?;
            value
                .map(|raw| {
                    let record: AliasRecord = serde_json::from_str(&raw)
                        .map_err(|_| provider_invalid("decode session alias"))?;
                    Ok(gateway_core::provider_ports::ProviderSessionAlias {
                        session_key: ProviderSessionAffinityKey::try_new(record.session_key)?,
                        follow_only: record.follow_only,
                        root_session_key: record
                            .root_session_key
                            .map(ProviderSessionAffinityKey::try_new)
                            .transpose()?,
                    })
                })
                .transpose()
        })
    }

    fn bind_alias<'a>(
        &'a self,
        provider: &'a ProviderKind,
        alias: &'a ProviderSessionAffinityKey,
        session: &'a gateway_core::provider_ports::ProviderSessionAlias,
        ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let record = serde_json::to_string(&AliasRecord {
                session_key: session.session_key.expose_to_store().to_owned(),
                follow_only: session.follow_only,
                root_session_key: session
                    .root_session_key
                    .as_ref()
                    .map(|key| key.expose_to_store().to_owned()),
            })
            .map_err(|_| provider_invalid("encode session alias"))?;
            redis::Script::new("local current = redis.call('GET', KEYS[1]); if not current or current == ARGV[1] then redis.call('PSETEX', KEYS[1], tonumber(ARGV[3]), ARGV[2]); return 1 end; return 0")
                .key(format!("{}:alias", self.key(provider, alias)?))
                // 缺失与同目标均可写入，冲突不能覆盖另一会话
                .arg(&record)
                .arg(&record)
                .arg(session_affinity_ttl_millis(ttl)?)
                .invoke_async::<bool>(&mut self.connection.clone()).await
                .map_err(|source| crate::provider_unavailable("bind session alias", source))
        })
    }
}

fn session_affinity_ttl_millis(ttl: Duration) -> Result<u64, ProviderStoreError> {
    if ttl.is_zero() || ttl > MAX_SESSION_AFFINITY_TTL {
        return Err(provider_invalid("validate provider session affinity TTL"));
    }
    u64::try_from(ttl.as_millis())
        .map_err(|_| provider_invalid("validate provider session affinity TTL"))
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
