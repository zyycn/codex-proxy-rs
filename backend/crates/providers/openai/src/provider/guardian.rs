//! 按 Client Key 和账号关联父响应的连接路由快照，不持久化 Cookie 或改变选号

use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gateway_core::{account::ProviderAccount, policy::ClientApiKeyId};
use lru::LruCache;
use sha2::{Digest, Sha256};

use crate::transport::routing_affinity::ChatGptRoutingAffinity;

use gateway_protocol::openai::CODEX_AUTO_REVIEW_MODEL;

pub(super) fn review_model(
    enabled: bool,
    model: &gateway_core::routing::PublicModelId,
    operation: &gateway_core::operation::Operation,
    previous: Option<&gateway_core::operation::ProviderSessionState>,
) -> Option<gateway_core::routing::UpstreamModelId> {
    let gateway_core::operation::Operation::Generate(request) = operation else {
        return None;
    };
    let raw_model = request.protocol_payload().body().get("model")?.as_str()?;
    if model.as_str() != raw_model || raw_model == CODEX_AUTO_REVIEW_MODEL || !is_reviewer(request)
    {
        return None;
    }
    // 已确认的续接沿用上一跳的实际模型，只有新审核使用本次请求冻结的设置
    let previous_model = previous
        .and_then(|state| state.payload().get("reviewer_model"))
        .and_then(serde_json::Value::as_str);
    previous_model
        .map_or(enabled, |previous| previous == CODEX_AUTO_REVIEW_MODEL)
        .then(|| {
            gateway_core::routing::UpstreamModelId::new(CODEX_AUTO_REVIEW_MODEL)
                .expect("valid reviewer model")
        })
}

pub(super) fn is_reviewer(request: &gateway_core::operation::GenerateRequest) -> bool {
    use gateway_protocol::openai::{CodexGuardianRole, OPENAI_PROTOCOL, codex_guardian_role};
    let payload = request.protocol_payload();
    payload.protocol() == OPENAI_PROTOCOL
        && codex_guardian_role(payload.body(), payload.context())
            == Some(CodexGuardianRole::Reviewer)
}

pub(super) fn continuation_model_matches(
    previous: Option<&str>,
    next: &str,
    direct_model: bool,
) -> bool {
    // 外部及旧版记录没有已确认的审核模型，不能将其原生 handle 交给自动选择的目标
    previous.map_or(!direct_model, |previous| previous == next)
}

// 与连接续接 tombstone 使用相同保留窗口；最多 4096 份、每份两个 8 KiB Cookie
const RETENTION: Duration = Duration::from_mins(30);
const MAX_PARENT_RESPONSES: usize = 4096;

#[derive(Clone)]
pub(super) struct GuardianRouting {
    parents: Arc<Mutex<LruCache<[u8; 32], ParentRouting>>>,
}

#[derive(Clone)]
pub(super) struct ParentRouting {
    captured_at: Instant,
    pub(super) affinity: Option<ChatGptRoutingAffinity>,
}

impl Default for GuardianRouting {
    fn default() -> Self {
        Self {
            parents: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(MAX_PARENT_RESPONSES).expect("positive parent limit"),
            ))),
        }
    }
}

impl GuardianRouting {
    pub(super) fn get(
        &self,
        client_key: &ClientApiKeyId,
        account: &ProviderAccount,
        response_id: &str,
    ) -> Option<ParentRouting> {
        let key = parent_key(client_key, account, response_id);
        let mut parents = self
            .parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let parent = parents.peek(&key)?;
        if parent.captured_at.elapsed() >= RETENTION {
            parents.pop(&key);
            return None;
        }
        Some(parent.clone())
    }

    pub(super) fn remember(
        &self,
        client_key: &ClientApiKeyId,
        account: &ProviderAccount,
        response_id: &str,
        affinity: Option<&ChatGptRoutingAffinity>,
    ) {
        let key = parent_key(client_key, account, response_id);
        let mut parents = self
            .parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if parents.contains(&key) {
            return;
        }
        parents.put(
            key,
            ParentRouting {
                captured_at: Instant::now(),
                affinity: affinity.cloned(),
            },
        );
    }
}

fn parent_key(
    client_key: &ClientApiKeyId,
    account: &ProviderAccount,
    response_id: &str,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    let identity = crate::request_identity::oauth_identity_key(account);
    for part in [client_key.as_str(), &identity, response_id] {
        hash.update(part.len().to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().into()
}
