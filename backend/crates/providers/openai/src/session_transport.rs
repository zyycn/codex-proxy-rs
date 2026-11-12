//! 按客户端线程维护 WS 失败预算，空闲过期后释放状态

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use gateway_core::provider_ports::ProviderSessionAffinityKey;
use tokio::time::Instant;

const SESSION_IDLE_RETENTION: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_SESSION_TRANSPORTS: usize = 16_384;

#[derive(Clone, Default)]
pub(crate) struct CodexSessionTransportRecovery {
    sessions: Arc<Mutex<HashMap<ProviderSessionAffinityKey, SessionTransport>>>,
}

struct SessionTransport {
    state: TransportState,
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
        self.update(key, |session| {
            session.state = TransportState::WebSocket { failures: 0 }
        });
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
                last_used: now,
            });
        let result = update(&mut session);
        if matches!(session.state, TransportState::WebSocket { failures: 0 }) {
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
