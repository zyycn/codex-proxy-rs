//! OpenAI 根会话级上游传输降级状态。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gateway_core::provider_ports::ProviderSessionAffinityKey;

use crate::credential::CODEX_ROOT_SESSION_TTL;

/// Provider 是进程级单例；限制不可显式观测结束的客户端会话占用。
const MAX_SESSION_HTTP_FALLBACKS: usize = 16_384;

#[derive(Clone)]
pub(crate) struct CodexSessionTransportFallbacks {
    state: Arc<Mutex<SessionTransportFallbackState>>,
    ttl: Duration,
    capacity: usize,
}

#[derive(Default)]
struct SessionTransportFallbackState {
    http_only: HashMap<ProviderSessionAffinityKey, Instant>,
}

impl Default for CodexSessionTransportFallbacks {
    fn default() -> Self {
        Self::with_limits(CODEX_ROOT_SESSION_TTL, MAX_SESSION_HTTP_FALLBACKS)
    }
}

impl CodexSessionTransportFallbacks {
    fn with_limits(ttl: Duration, capacity: usize) -> Self {
        debug_assert!(!ttl.is_zero());
        debug_assert!(capacity > 0);
        Self {
            state: Arc::new(Mutex::new(SessionTransportFallbackState::default())),
            ttl,
            capacity,
        }
    }

    /// 返回该根会话是否已粘性切到 HTTP，并为活跃会话续期。
    pub(crate) fn is_http_only(&self, key: &ProviderSessionAffinityKey) -> bool {
        self.is_http_only_at(key, Instant::now())
    }

    /// 在当前 WebSocket 重试预算耗尽后，为根会话启用粘性 HTTP。
    ///
    /// 返回 `true` 表示本次发生了禁用状态转换；已禁用会话只续期。
    pub(crate) fn disable_websocket(&self, key: &ProviderSessionAffinityKey) -> bool {
        self.disable_websocket_at(key, Instant::now())
    }

    fn is_http_only_at(&self, key: &ProviderSessionAffinityKey, now: Instant) -> bool {
        let mut state = self.lock();
        let Some(last_seen) = state.http_only.get_mut(key) else {
            return false;
        };
        if now.saturating_duration_since(*last_seen) >= self.ttl {
            state.http_only.remove(key);
            return false;
        }
        *last_seen = now;
        true
    }

    fn disable_websocket_at(&self, key: &ProviderSessionAffinityKey, now: Instant) -> bool {
        let mut state = self.lock();
        let already_disabled = state
            .http_only
            .get(key)
            .is_some_and(|last_seen| now.saturating_duration_since(*last_seen) < self.ttl);
        if !already_disabled && !state.http_only.contains_key(key) {
            self.make_room(&mut state, now);
        }
        state.http_only.insert(key.clone(), now);
        !already_disabled
    }

    fn make_room(&self, state: &mut SessionTransportFallbackState, now: Instant) {
        if state.http_only.len() < self.capacity {
            return;
        }
        state
            .http_only
            .retain(|_, last_seen| now.saturating_duration_since(*last_seen) < self.ttl);
        if state.http_only.len() < self.capacity {
            return;
        }
        let oldest = state
            .http_only
            .iter()
            .min_by_key(|(_, last_seen)| **last_seen)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            state.http_only.remove(&oldest);
        }
    }

    fn lock(&self) -> MutexGuard<'_, SessionTransportFallbackState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
