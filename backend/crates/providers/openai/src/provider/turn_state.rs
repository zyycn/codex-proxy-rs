//! 只为上游已返回的 turn state 保存归属证明，不恢复正文或参与账号选择

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gateway_core::engine::AttemptContext;
use sha2::{Digest, Sha256};

use crate::{
    credential::{CodexCredentialLease, CodexRuntimeAuthentication},
    transport::protocol::responses::CodexResponsesRequest,
};

// 仅保存 32 字节摘要与过期时间，4096 项的条目数据约 192 KiB，不随 token 长度增长
const MAX_PROOFS: usize = 4096;

#[derive(Clone, Default)]
pub(super) struct TurnStateOwnership {
    proofs: Arc<Mutex<HashMap<[u8; 32], Instant>>>,
}

pub(super) struct TurnStateScope {
    ownership: TurnStateOwnership,
    identity: Sha256,
    retention: Duration,
}

impl TurnStateOwnership {
    pub(super) fn scope(
        &self,
        context: &AttemptContext,
        lease: &CodexCredentialLease,
        request: &CodexResponsesRequest,
    ) -> Option<TurnStateScope> {
        let turn_id = request
            .client_turn_id
            .as_deref()
            .filter(|id| !id.is_empty())?;
        let mut identity = Sha256::new();
        for part in [
            context.client_api_key_ref().as_str(),
            lease.account_id().as_str(),
            request.client_session_id.as_deref().unwrap_or_default(),
            request.client_thread_id.as_deref().unwrap_or_default(),
            turn_id,
        ] {
            identity.update(part.len().to_le_bytes());
            identity.update(part.as_bytes());
        }
        // OAuth 刷新不改变账号归属；API Key 替换则不能继承原认证的路由状态
        if matches!(
            lease.authentication(),
            CodexRuntimeAuthentication::ApiKey(_)
        ) {
            identity.update([1]);
            identity.update(lease.account().revision().get().to_le_bytes());
        } else {
            identity.update([0]);
        }
        Some(TurnStateScope {
            ownership: self.clone(),
            identity,
            retention: context
                .account_selection_policy()
                .openai_session_affinity_ttl(),
        })
    }
}

impl TurnStateScope {
    pub(super) fn owns(&self, token: &str) -> bool {
        let key = self.key(token);
        let mut proofs = self
            .ownership
            .proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(expires_at) = proofs.get_mut(&key) else {
            return false;
        };
        let now = Instant::now();
        if *expires_at <= now {
            proofs.remove(&key);
            return false;
        }
        *expires_at = now + self.retention;
        true
    }

    pub(super) fn remember(&self, token: &str) {
        if token.is_empty() {
            return;
        }
        let key = self.key(token);
        let now = Instant::now();
        let mut proofs = self
            .ownership
            .proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if proofs.len() >= MAX_PROOFS && !proofs.contains_key(&key) {
            proofs.retain(|_, expires_at| *expires_at > now);
            if proofs.len() >= MAX_PROOFS
                && let Some(oldest) = proofs
                    .iter()
                    .min_by_key(|(_, expires_at)| **expires_at)
                    .map(|(key, _)| *key)
            {
                proofs.remove(&oldest);
            }
        }
        proofs.insert(key, now + self.retention);
    }

    fn key(&self, token: &str) -> [u8; 32] {
        let mut hash = self.identity.clone();
        hash.update(token.len().to_le_bytes());
        hash.update(token.as_bytes());
        hash.finalize().into()
    }
}
