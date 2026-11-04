//! 按客户端线程维护 WS 失败预算及大包 HTTP 回退区间，空闲过期后释放状态

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use gateway_core::provider_ports::ProviderSessionAffinityKey;
use tokio::time::Instant;

// 在实测的 16 MiB 上游 WS 边界前留半 MiB，再用 2 MiB 回差避免频繁切换
const MIB: usize = 1024 * 1024;
pub(crate) const WS_PAYLOAD_LIMIT: usize = 15 * MIB + MIB / 2;
const WS_PAYLOAD_RESUME: usize = WS_PAYLOAD_LIMIT - 2 * MIB;

const SESSION_IDLE_RETENTION: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_SESSION_TRANSPORTS: usize = 16_384;

#[derive(Clone, Default)]
pub(crate) struct CodexSessionTransportRecovery {
    sessions: Arc<Mutex<HashMap<ProviderSessionAffinityKey, SessionTransport>>>,
}

struct SessionTransport {
    state: TransportState,
    oversized: bool,
    last_used: Instant,
}

enum TransportState {
    WebSocket { failures: u32 },
    Http,
}

impl CodexSessionTransportRecovery {
    pub(crate) fn uses_http(&self, key: &ProviderSessionAffinityKey) -> bool {
        let now = Instant::now();
        let mut sessions = self.lock();
        let Some(session) = sessions.get_mut(key) else {
            return false;
        };
        if now.saturating_duration_since(session.last_used) >= SESSION_IDLE_RETENTION {
            sessions.remove(key);
            return false;
        }
        session.last_used = now;
        matches!(session.state, TransportState::Http)
    }

    /// 返回连续失败是否已耗尽首发加重试预算
    pub(crate) fn record_websocket_failure(
        &self,
        key: &ProviderSessionAffinityKey,
        max_retries: u32,
    ) -> bool {
        self.update(key, |session| {
            session.state = match session.state {
                TransportState::WebSocket { failures } if failures < max_retries => {
                    TransportState::WebSocket {
                        failures: failures + 1,
                    }
                }
                _ => TransportState::Http,
            };
            matches!(session.state, TransportState::Http)
        })
    }

    pub(crate) fn disable_websocket(&self, key: &ProviderSessionAffinityKey) {
        self.update(key, |session| session.state = TransportState::Http);
    }

    pub(crate) fn websocket_succeeded(&self, key: &ProviderSessionAffinityKey) {
        // 预热或增量成功不能证明下一份完整请求能放进 WS
        self.update(key, |session| {
            session.state = TransportState::WebSocket { failures: 0 }
        });
    }

    /// 只用完整新链的实际帧长度更新大小偏好，重连沿用同一线程的判断
    pub(crate) fn http_for_full_payload(
        &self,
        key: Option<&ProviderSessionAffinityKey>,
        bytes: usize,
    ) -> bool {
        let Some(key) = key else {
            return bytes >= WS_PAYLOAD_LIMIT;
        };
        self.update(key, |session| {
            if bytes >= WS_PAYLOAD_LIMIT {
                session.oversized = true;
            } else if bytes < WS_PAYLOAD_RESUME {
                session.oversized = false;
            }
            session.oversized
        })
    }

    fn update<T>(
        &self,
        key: &ProviderSessionAffinityKey,
        update: impl FnOnce(&mut SessionTransport) -> T,
    ) -> T {
        let now = Instant::now();
        let mut sessions = self.lock();
        let mut session = sessions
            .remove(key)
            .filter(|s| now.saturating_duration_since(s.last_used) < SESSION_IDLE_RETENTION)
            .unwrap_or(SessionTransport {
                state: TransportState::WebSocket { failures: 0 },
                oversized: false,
                last_used: now,
            });
        let result = update(&mut session);
        if !session.oversized && matches!(session.state, TransportState::WebSocket { failures: 0 })
        {
            return result;
        }
        session.last_used = now;
        if sessions.len() >= MAX_SESSION_TRANSPORTS {
            sessions.retain(|_, session| {
                now.saturating_duration_since(session.last_used) < SESSION_IDLE_RETENTION
            });
            if sessions.len() >= MAX_SESSION_TRANSPORTS
                && let Some(oldest) = sessions
                    .iter()
                    .min_by_key(|(_, session)| session.last_used)
                    .map(|(key, _)| key.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(key.clone(), session);
        result
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<ProviderSessionAffinityKey, SessionTransport>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
