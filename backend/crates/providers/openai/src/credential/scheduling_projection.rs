//! 原生会话选号的短时完整候选投影与有界读取合并

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use gateway_core::account::scope::FrozenAccountScope;
use gateway_core::account::{AccountCandidate, AccountSelectionPolicy};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::ProviderConcurrencyPool;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

const PROJECTION_TTL: Duration = Duration::from_millis(200);
const MAX_PROJECTIONS: usize = 32;
const MAX_CACHED_CANDIDATES: usize = 8192;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SchedulingProjectionKey {
    pub client_key: ClientApiKeyId,
    pub scope: Arc<FrozenAccountScope>,
    pub model: String,
    pub policy: AccountSelectionPolicy,
    pub requires_websocket: bool,
    pub concurrency_pool: ProviderConcurrencyPool,
}

struct Projection {
    key: SchedulingProjectionKey,
    observed_at: Instant,
    candidates: Arc<[AccountCandidate]>,
}

/// 投影只用于复用完整池观测，准入账号仍由选择器实时复核
#[derive(Default)]
pub(super) struct SchedulingProjectionCache {
    entries: Mutex<VecDeque<Projection>>,
    fills: Mutex<Vec<(SchedulingProjectionKey, Weak<AsyncMutex<()>>)>>,
}

impl SchedulingProjectionCache {
    /// 缺失时合并已有会话的完整读取，取消持有者会释放填充锁
    pub async fn read(
        &self,
        key: &SchedulingProjectionKey,
    ) -> (Option<Arc<[AccountCandidate]>>, Option<OwnedMutexGuard<()>>) {
        if let Some(candidates) = self.get(key) {
            return (Some(candidates), None);
        }
        let Some(fill) = self.fill_for(key) else {
            return (None, None);
        };
        let fill = fill.lock_owned().await;
        if let Some(candidates) = self.get(key) {
            return (Some(candidates), None);
        }
        (None, Some(fill))
    }

    fn fill_for(&self, key: &SchedulingProjectionKey) -> Option<Arc<AsyncMutex<()>>> {
        let mut fills = self
            .fills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fills.retain(|(_, fill)| fill.strong_count() > 0);
        if let Some(fill) = fills
            .iter()
            .find(|(existing, _)| existing == key)
            .and_then(|(_, fill)| fill.upgrade())
        {
            return Some(fill);
        }
        // 不同范围独立填充，合并槽位耗尽时直接查询，取消后弱引用可回收
        if fills.len() >= MAX_PROJECTIONS {
            return None;
        }
        let fill = Arc::new(AsyncMutex::new(()));
        fills.push((key.clone(), Arc::downgrade(&fill)));
        Some(fill)
    }

    fn get(&self, key: &SchedulingProjectionKey) -> Option<Arc<[AccountCandidate]>> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|entry| entry.observed_at.elapsed() < PROJECTION_TTL);
        entries
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| Arc::clone(&entry.candidates))
    }

    pub fn publish(
        &self,
        key: SchedulingProjectionKey,
        observed_at: Instant,
        candidates: &[AccountCandidate],
    ) {
        // TTL 从读取开始计时，慢查询和超大账号池只服务当前请求
        if observed_at.elapsed() >= PROJECTION_TTL
            || candidates.is_empty()
            || candidates.len() > MAX_CACHED_CANDIDATES
        {
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|entry| entry.observed_at.elapsed() < PROJECTION_TTL);
        if entries
            .iter()
            .any(|entry| entry.key == key && entry.observed_at > observed_at)
        {
            return;
        }
        entries.retain(|entry| entry.key != key);
        let mut candidate_count = entries
            .iter()
            .map(|entry| entry.candidates.len())
            .sum::<usize>();
        while entries.len() >= MAX_PROJECTIONS
            || candidate_count + candidates.len() > MAX_CACHED_CANDIDATES
        {
            if let Some(removed) = entries.pop_front() {
                candidate_count -= removed.candidates.len();
            }
        }
        entries.push_back(Projection {
            key,
            observed_at,
            candidates: Arc::from(candidates),
        });
    }

    pub fn invalidate(&self, key: &SchedulingProjectionKey) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|entry| entry.key != *key);
    }
}
