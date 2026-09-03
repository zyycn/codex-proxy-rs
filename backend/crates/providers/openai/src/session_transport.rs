//! OpenAI 根会话级上游传输恢复状态。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use gateway_core::provider_ports::ProviderSessionAffinityKey;
use tokio::time::Instant;
use uuid::Uuid;

/// 未成功恢复的失败链需要跨过最长冷却，避免 4 小时档在半开前被清理。
const SESSION_RECOVERY_RETENTION: Duration = Duration::from_secs(8 * 60 * 60);
const HTTP_COOLDOWNS: [Duration; 4] = [
    Duration::from_secs(5 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(4 * 60 * 60),
];
/// Provider 是进程级单例；限制不可显式观测结束的客户端会话占用。
const MAX_SESSION_RECOVERIES: usize = 16_384;

#[derive(Clone)]
pub(crate) struct CodexSessionTransportRecovery {
    state: Arc<Mutex<SessionTransportRecoveryState>>,
    http_cooldowns: [Duration; 4],
    retention: Duration,
    capacity: usize,
}

#[derive(Default)]
struct SessionTransportRecoveryState {
    sessions: HashMap<ProviderSessionAffinityKey, SessionCircuit>,
}

enum SessionCircuit {
    FreshPending {
        failure_count: u8,
        last_failure_at: Instant,
    },
    Open {
        failure_count: u8,
        last_failure_at: Instant,
        until: Instant,
    },
    HalfOpen {
        failure_count: u8,
        last_failure_at: Instant,
        probe_id: Uuid,
    },
}

impl SessionCircuit {
    const fn failure_count(&self) -> u8 {
        match self {
            Self::FreshPending { failure_count, .. }
            | Self::Open { failure_count, .. }
            | Self::HalfOpen { failure_count, .. } => *failure_count,
        }
    }

    const fn last_failure_at(&self) -> Instant {
        match self {
            Self::FreshPending {
                last_failure_at, ..
            }
            | Self::Open {
                last_failure_at, ..
            }
            | Self::HalfOpen {
                last_failure_at, ..
            } => *last_failure_at,
        }
    }
}

impl Default for CodexSessionTransportRecovery {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(SessionTransportRecoveryState::default())),
            http_cooldowns: HTTP_COOLDOWNS,
            retention: SESSION_RECOVERY_RETENTION,
            capacity: MAX_SESSION_RECOVERIES,
        }
    }
}

#[derive(Default)]
pub(crate) enum CodexSessionTransportDecision {
    #[default]
    Default,
    FreshWebSocket {
        failure_count: u8,
        probe: CodexSessionWebSocketProbe,
    },
    HttpSse {
        reason: SessionHttpFallbackReason,
        failure_count: u8,
        retry_after: Option<Duration>,
    },
}

impl CodexSessionTransportDecision {
    pub(crate) const fn action(&self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::FreshWebSocket { .. } => Some("fresh_websocket_probe"),
            Self::HttpSse {
                reason: SessionHttpFallbackReason::Cooldown,
                ..
            } => Some("http_cooldown"),
            Self::HttpSse {
                reason: SessionHttpFallbackReason::ProbeInFlight,
                ..
            } => Some("http_probe_in_flight"),
        }
    }

    pub(crate) const fn failure_count(&self) -> Option<u8> {
        match self {
            Self::Default => None,
            Self::FreshWebSocket { failure_count, .. } | Self::HttpSse { failure_count, .. } => {
                Some(*failure_count)
            }
        }
    }

    pub(crate) const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::HttpSse { retry_after, .. } => *retry_after,
            Self::Default | Self::FreshWebSocket { .. } => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SessionWebSocketFallback {
    RetryBudgetExhausted,
    UpgradeRequired,
}

impl SessionWebSocketFallback {
    const fn minimum_failure_count(self) -> u8 {
        match self {
            Self::RetryBudgetExhausted => 2,
            Self::UpgradeRequired => 5,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SessionHttpFallbackReason {
    Cooldown,
    ProbeInFlight,
}

#[derive(Clone, Copy)]
pub(crate) struct CodexSessionRecoveryTransition {
    failure_count: u8,
    cooldown: Option<Duration>,
}

impl CodexSessionRecoveryTransition {
    pub(crate) const fn action(self) -> &'static str {
        if self.cooldown.is_some() {
            "http_cooldown"
        } else {
            "fresh_websocket_next"
        }
    }

    pub(crate) const fn failure_count(self) -> u8 {
        self.failure_count
    }

    pub(crate) const fn cooldown(self) -> Option<Duration> {
        self.cooldown
    }
}

/// 一次根会话级 fresh WebSocket 许可。
///
/// 许可未得到明确结果就被释放时，恢复状态会重新允许下一次半开探测。
pub(crate) struct CodexSessionWebSocketProbe {
    recovery: CodexSessionTransportRecovery,
    key: ProviderSessionAffinityKey,
    probe_id: Uuid,
    armed: bool,
}

impl CodexSessionWebSocketProbe {
    fn new(
        recovery: CodexSessionTransportRecovery,
        key: ProviderSessionAffinityKey,
        probe_id: Uuid,
    ) -> Self {
        Self {
            recovery,
            key,
            probe_id,
            armed: true,
        }
    }

    pub(crate) fn succeed(mut self) -> bool {
        let recovered = self.recovery.record_probe_success(&self.key, self.probe_id);
        self.armed = false;
        recovered
    }

    pub(crate) fn post_send_failed(mut self) -> CodexSessionRecoveryTransition {
        let transition = self.recovery.record_post_send_failure(&self.key);
        self.armed = false;
        transition
    }

    pub(crate) fn fallback(
        mut self,
        fallback: SessionWebSocketFallback,
    ) -> CodexSessionRecoveryTransition {
        let transition = self.recovery.record_websocket_fallback(&self.key, fallback);
        self.armed = false;
        transition
    }
}

impl Drop for CodexSessionWebSocketProbe {
    fn drop(&mut self) {
        if self.armed {
            self.recovery.cancel_probe(&self.key, self.probe_id);
        }
    }
}

impl CodexSessionTransportRecovery {
    pub(crate) fn decide(&self, key: &ProviderSessionAffinityKey) -> CodexSessionTransportDecision {
        self.decide_at(key, Instant::now())
    }

    pub(crate) fn record_post_send_failure(
        &self,
        key: &ProviderSessionAffinityKey,
    ) -> CodexSessionRecoveryTransition {
        self.record_post_send_failure_at(key, Instant::now())
    }

    pub(crate) fn record_websocket_fallback(
        &self,
        key: &ProviderSessionAffinityKey,
        fallback: SessionWebSocketFallback,
    ) -> CodexSessionRecoveryTransition {
        self.record_websocket_fallback_at(key, fallback, Instant::now())
    }

    fn decide_at(
        &self,
        key: &ProviderSessionAffinityKey,
        now: Instant,
    ) -> CodexSessionTransportDecision {
        let mut state = self.lock();
        let stale = state.sessions.get(key).is_some_and(|circuit| {
            now.saturating_duration_since(circuit.last_failure_at()) >= self.retention
        });
        if stale {
            state.sessions.remove(key);
            return CodexSessionTransportDecision::Default;
        }

        let Some(circuit) = state.sessions.get_mut(key) else {
            return CodexSessionTransportDecision::Default;
        };
        match circuit {
            SessionCircuit::Open {
                failure_count,
                until,
                ..
            } if now < *until => CodexSessionTransportDecision::HttpSse {
                reason: SessionHttpFallbackReason::Cooldown,
                failure_count: *failure_count,
                retry_after: Some(until.saturating_duration_since(now)),
            },
            SessionCircuit::HalfOpen { failure_count, .. } => {
                CodexSessionTransportDecision::HttpSse {
                    reason: SessionHttpFallbackReason::ProbeInFlight,
                    failure_count: *failure_count,
                    retry_after: None,
                }
            }
            SessionCircuit::FreshPending {
                failure_count,
                last_failure_at,
            }
            | SessionCircuit::Open {
                failure_count,
                last_failure_at,
                ..
            } => {
                let failure_count = *failure_count;
                let last_failure_at = *last_failure_at;
                let probe_id = Uuid::new_v4();
                *circuit = SessionCircuit::HalfOpen {
                    failure_count,
                    last_failure_at,
                    probe_id,
                };
                CodexSessionTransportDecision::FreshWebSocket {
                    failure_count,
                    probe: CodexSessionWebSocketProbe::new(self.clone(), key.clone(), probe_id),
                }
            }
        }
    }

    fn record_post_send_failure_at(
        &self,
        key: &ProviderSessionAffinityKey,
        now: Instant,
    ) -> CodexSessionRecoveryTransition {
        let mut state = self.lock();
        let failure_count = self.next_failure_count(state.sessions.get(key), now);
        if !state.sessions.contains_key(key) {
            self.make_room(&mut state, now);
        }
        if failure_count == 1 {
            state.sessions.insert(
                key.clone(),
                SessionCircuit::FreshPending {
                    failure_count,
                    last_failure_at: now,
                },
            );
            return CodexSessionRecoveryTransition {
                failure_count,
                cooldown: None,
            };
        }

        let cooldown = self.cooldown_for(failure_count);
        state.sessions.insert(
            key.clone(),
            SessionCircuit::Open {
                failure_count,
                last_failure_at: now,
                until: now + cooldown,
            },
        );
        CodexSessionRecoveryTransition {
            failure_count,
            cooldown: Some(cooldown),
        }
    }

    fn record_websocket_fallback_at(
        &self,
        key: &ProviderSessionAffinityKey,
        fallback: SessionWebSocketFallback,
        now: Instant,
    ) -> CodexSessionRecoveryTransition {
        let mut state = self.lock();
        let failure_count = self
            .next_failure_count(state.sessions.get(key), now)
            .max(fallback.minimum_failure_count());
        if !state.sessions.contains_key(key) {
            self.make_room(&mut state, now);
        }
        let cooldown = self.cooldown_for(failure_count);
        state.sessions.insert(
            key.clone(),
            SessionCircuit::Open {
                failure_count,
                last_failure_at: now,
                until: now + cooldown,
            },
        );
        CodexSessionRecoveryTransition {
            failure_count,
            cooldown: Some(cooldown),
        }
    }

    fn next_failure_count(&self, circuit: Option<&SessionCircuit>, now: Instant) -> u8 {
        circuit.map_or(1, |circuit| {
            if now.saturating_duration_since(circuit.last_failure_at()) < self.retention {
                circuit.failure_count().saturating_add(1)
            } else {
                1
            }
        })
    }

    fn cooldown_for(&self, failure_count: u8) -> Duration {
        let index = usize::from(failure_count.saturating_sub(2)).min(self.http_cooldowns.len() - 1);
        self.http_cooldowns[index]
    }

    fn record_probe_success(&self, key: &ProviderSessionAffinityKey, probe_id: Uuid) -> bool {
        let mut state = self.lock();
        let owns_probe = matches!(
            state.sessions.get(key),
            Some(SessionCircuit::HalfOpen {
                probe_id: active_probe_id,
                ..
            }) if *active_probe_id == probe_id
        );
        if owns_probe {
            state.sessions.remove(key);
        }
        owns_probe
    }

    fn cancel_probe(&self, key: &ProviderSessionAffinityKey, probe_id: Uuid) {
        let mut state = self.lock();
        let Some(SessionCircuit::HalfOpen {
            failure_count,
            last_failure_at,
            probe_id: active_probe_id,
        }) = state.sessions.get(key)
        else {
            return;
        };
        if *active_probe_id != probe_id {
            return;
        }
        let failure_count = *failure_count;
        let last_failure_at = *last_failure_at;
        state.sessions.insert(
            key.clone(),
            SessionCircuit::FreshPending {
                failure_count,
                last_failure_at,
            },
        );
    }

    fn make_room(&self, state: &mut SessionTransportRecoveryState, now: Instant) {
        if state.sessions.len() < self.capacity {
            return;
        }
        state.sessions.retain(|_, circuit| {
            now.saturating_duration_since(circuit.last_failure_at()) < self.retention
        });
        if state.sessions.len() < self.capacity {
            return;
        }
        let oldest = state
            .sessions
            .iter()
            .min_by_key(|(_, circuit)| circuit.last_failure_at())
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            state.sessions.remove(&oldest);
        }
    }

    fn lock(&self) -> MutexGuard<'_, SessionTransportRecoveryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
