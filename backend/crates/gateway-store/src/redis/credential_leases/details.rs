//! 有界读取当前账号租约明细；事实随租约续期，进程退出后由 TTL 回收

use chrono::{DateTime, Utc};
use gateway_admin::model::accounts::{AccountConcurrencyEntry, AccountConcurrencyPage};
use gateway_core::provider_ports::ProviderLeaseMetadata;
use redis::Script;
use serde::{Deserialize, Serialize};

use super::invalid;
use super::{CredentialLeaseRequest, CredentialLeaseScope, RedisCredentialLeaseRepository};
use crate::{StoreResult, redis_unavailable};

// 用过期成员的数量定位有效区间，深分页也不遍历 offset 个成员
// 不在读端清理整个过期区间，避免大量过期租约阻塞本次有界查询
const PAGE_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
local expired = redis.call('ZCOUNT', KEYS[1], '-inf', now_ms)
local total = redis.call('ZCARD', KEYS[1]) - expired
local first = expired + tonumber(ARGV[1])
local members = redis.call('ZRANGE', KEYS[1], first, first + tonumber(ARGV[2]) - 1)
local rows = {}
for _, member in ipairs(members) do
    local lease_id = string.match(member, '^[^|]+|([^|]+)|')
    local key = KEYS[1] .. ':detail:' .. lease_id
    table.insert(rows, {lease_id, redis.call('HGET', key, 'facts') or '', redis.call('HGET', key, 'acquired_at') or ''})
end
return {tostring(now_ms), tostring(total), rows}
"#;

#[derive(Serialize, Deserialize)]
struct Detail {
    request_id: String,
    client_key_id: String,
    client_key_name: Option<String>,
    model: Option<String>,
    client_ip: Option<String>,
    started_at_ms: i64,
}

pub(super) fn detail_key(active: &str, lease_id: &str) -> String {
    format!("{active}:detail:{lease_id}")
}

pub(super) fn encode(metadata: Option<&ProviderLeaseMetadata>) -> StoreResult<String> {
    metadata.map_or_else(
        || Ok(String::new()),
        |metadata| {
            serde_json::to_string(&Detail {
                request_id: metadata.request_id.as_str().to_owned(),
                client_key_id: metadata.client_key_id.as_str().to_owned(),
                client_key_name: metadata.client_key_name.clone(),
                model: metadata.model.clone(),
                client_ip: metadata.client_ip.map(|ip| ip.to_string()),
                started_at_ms: DateTime::<Utc>::from(metadata.started_at).timestamp_millis(),
            })
            .map_err(|_| invalid("invalid lease metadata"))
        },
    )
}

impl RedisCredentialLeaseRepository {
    /// 与管理端并发计数一致，只读取普通池；页大小在存储边界再次限制
    pub async fn account_concurrency(
        &self,
        account_id: &str,
        offset: u64,
        limit: u32,
    ) -> StoreResult<AccountConcurrencyPage> {
        if !(1..=100).contains(&limit) || offset > u64::from(u32::MAX) {
            return Err(invalid("invalid concurrency page"));
        }
        let keys = self.keys(&CredentialLeaseRequest {
            scope: CredentialLeaseScope::ProviderAccount,
            resource_id: account_id.to_owned(),
            owner_id: "admin-concurrency".to_owned(),
            ttl: std::time::Duration::from_secs(1),
        })?;
        let (now_ms, total, rows): (i64, u64, Vec<(String, String, String)>) =
            Script::new(PAGE_SCRIPT)
                .key(&keys[0])
                .arg(offset)
                .arg(limit)
                .invoke_async(&mut self.connection.clone())
                .await
                .map_err(|source| redis_unavailable("read account concurrency", source))?;
        let observed_at = DateTime::from_timestamp_millis(now_ms)
            .ok_or_else(|| invalid("invalid lease observation time"))?;
        let entries = rows
            .into_iter()
            .map(|(lease_id, json, acquired_at)| {
                let detail = (!json.is_empty())
                    .then(|| serde_json::from_str::<Detail>(&json))
                    .transpose()
                    .map_err(|_| invalid("invalid lease metadata"))?;
                Ok(match detail {
                    Some(detail) => AccountConcurrencyEntry {
                        lease_id,
                        request_id: Some(detail.request_id),
                        client_key_id: Some(detail.client_key_id),
                        client_key_name: detail.client_key_name,
                        model: detail.model,
                        client_ip: detail.client_ip,
                        started_at: DateTime::from_timestamp_millis(detail.started_at_ms),
                        acquired_at: Some(super::timestamp(&acquired_at)?),
                    },
                    // 租约是计数依据；缺失可丢失元数据时仍保留真实占用
                    None => AccountConcurrencyEntry {
                        lease_id,
                        request_id: None,
                        client_key_id: None,
                        client_key_name: None,
                        model: None,
                        client_ip: None,
                        started_at: None,
                        acquired_at: None,
                    },
                })
            })
            .collect::<StoreResult<_>>()?;
        Ok(AccountConcurrencyPage {
            entries,
            total,
            observed_at,
        })
    }
}
