//! 按 Client Key 和账号关联父响应的连接路由快照，不持久化 Cookie 或改变选号

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gateway_core::{account::ProviderAccountId, policy::ClientApiKeyId};
use sha2::{Digest, Sha256};

use crate::transport::routing_affinity::ChatGptRoutingAffinity;

// 与连接续接 tombstone 使用相同保留窗口；最多 4096 份、每份两个 8 KiB Cookie
const RETENTION: Duration = Duration::from_mins(30);
const MAX_PARENT_RESPONSES: usize = 4096;

#[derive(Clone, Default)]
pub(super) struct GuardianRouting {
    parents: Arc<Mutex<HashMap<[u8; 32], ParentRouting>>>,
}

struct ParentRouting {
    captured_at: Instant,
    affinity: ChatGptRoutingAffinity,
}

impl GuardianRouting {
    pub(super) fn get(
        &self,
        client_key: &ClientApiKeyId,
        account: &ProviderAccountId,
        response_id: &str,
    ) -> Option<ChatGptRoutingAffinity> {
        let key = parent_key(client_key, account, response_id);
        let mut parents = self
            .parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let parent = parents.get(&key)?;
        if parent.captured_at.elapsed() >= RETENTION {
            parents.remove(&key);
            return None;
        }
        Some(parent.affinity.clone())
    }

    pub(super) fn remember(
        &self,
        client_key: &ClientApiKeyId,
        account: &ProviderAccountId,
        response_id: &str,
        affinity: &ChatGptRoutingAffinity,
    ) {
        let key = parent_key(client_key, account, response_id);
        let mut parents = self
            .parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if parents.contains_key(&key) {
            return;
        }
        if parents.len() >= MAX_PARENT_RESPONSES {
            parents.retain(|_, parent| parent.captured_at.elapsed() < RETENTION);
            if parents.len() >= MAX_PARENT_RESPONSES
                && let Some(oldest) = parents
                    .iter()
                    .min_by_key(|(_, parent)| parent.captured_at)
                    .map(|(key, _)| *key)
            {
                parents.remove(&oldest);
            }
        }
        parents.insert(
            key,
            ParentRouting {
                captured_at: Instant::now(),
                affinity: affinity.clone(),
            },
        );
    }
}

fn parent_key(
    client_key: &ClientApiKeyId,
    account: &ProviderAccountId,
    response_id: &str,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in [client_key.as_str(), account.as_str(), response_id] {
        hash.update(part.len().to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().into()
}
