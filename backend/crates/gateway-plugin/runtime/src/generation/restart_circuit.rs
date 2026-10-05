//! 按插件实例身份累计重启失败，管理熔断窗口与尝试生命周期

use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::rpc::RpcSessionLifecycle;

const MAXIMUM_TRACKED_IDENTITIES: usize = 256;

/// 原生插件重启熔断的保守初始参数；数值可调，但不代表已完成性能定标
#[derive(Debug, Clone, Copy)]
pub struct PluginRestartCircuitConfig {
    pub maximum_failures: NonZeroUsize,
    pub stability_window: Duration,
}

impl Default for PluginRestartCircuitConfig {
    fn default() -> Self {
        Self {
            maximum_failures: NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN),
            stability_window: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RestartIdentity {
    instance_id: String,
    fingerprint: String,
}

impl RestartIdentity {
    pub(super) fn new(instance_id: String, fingerprint: String) -> Self {
        Self {
            instance_id,
            fingerprint,
        }
    }
}

struct FailureBudget {
    failures: usize,
    latest_attempt: u64,
    touched: u64,
}

#[derive(Clone)]
struct AttemptFailure {
    uptime: Duration,
}

#[derive(Default)]
struct Attempt {
    failure: Option<AttemptFailure>,
    failure_applied: bool,
}

#[derive(Default)]
struct RestartCircuitState {
    budgets: BTreeMap<RestartIdentity, FailureBudget>,
    attempts: BTreeMap<RestartIdentity, BTreeMap<u64, Attempt>>,
    next_attempt: u64,
    clock: u64,
}

pub(super) struct RestartCircuits {
    config: PluginRestartCircuitConfig,
    state: Mutex<RestartCircuitState>,
}

impl RestartCircuits {
    pub(super) fn new(config: PluginRestartCircuitConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: Mutex::new(RestartCircuitState::default()),
        })
    }

    pub(super) fn begin(
        self: &Arc<Self>,
        identity: RestartIdentity,
    ) -> Option<RpcSessionLifecycle> {
        let attempt = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if is_open(&state, &identity, self.config.maximum_failures) {
                return None;
            }
            state.next_attempt = state.next_attempt.wrapping_add(1);
            let attempt = state.next_attempt;
            state
                .attempts
                .entry(identity.clone())
                .or_default()
                .insert(attempt, Attempt::default());
            attempt
        };
        let failure_registry = Arc::clone(self);
        let failure_identity = identity.clone();
        let end_registry = Arc::clone(self);
        Some(RpcSessionLifecycle::new(
            move |uptime| {
                failure_registry.record_failure(&failure_identity, attempt, uptime);
            },
            move || end_registry.end(&identity, attempt),
        ))
    }

    pub(super) fn is_open(&self, identity: &RestartIdentity) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        is_open(&state, identity, self.config.maximum_failures)
    }

    fn record_failure(&self, identity: &RestartIdentity, attempt: u64, uptime: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let apply = state.attempts.get_mut(identity).and_then(|attempts| {
            let current = attempts.keys().next_back().copied();
            let entry = attempts.get_mut(&attempt)?;
            if entry.failure.is_none() {
                entry.failure = Some(AttemptFailure { uptime });
            }
            if current == Some(attempt) && !entry.failure_applied {
                entry.failure_applied = true;
                entry.failure.clone()
            } else {
                None
            }
        });
        if let Some(failure) = apply {
            self.apply_failure(&mut state, identity.clone(), attempt, failure);
        }
    }

    fn end(&self, identity: &RestartIdentity, attempt: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut restored = None;
        let mut remove_identity = false;
        if let Some(attempts) = state.attempts.get_mut(identity) {
            let was_current = attempts.keys().next_back().copied() == Some(attempt);
            attempts.remove(&attempt);
            if was_current
                && let Some((_, previous)) = attempts.last_key_value()
                && !previous.failure_applied
                && let Some(failure) = previous.failure.clone()
            {
                restored = attempts
                    .keys()
                    .next_back()
                    .copied()
                    .map(|attempt| (attempt, failure));
            }
            let previous = attempts.keys().next_back().copied();
            if restored.is_some()
                && let Some(previous) = previous.and_then(|key| attempts.get_mut(&key))
            {
                previous.failure_applied = true;
            }
            remove_identity = attempts.is_empty();
        }
        if remove_identity {
            state.attempts.remove(identity);
        }
        if let Some((attempt, failure)) = restored {
            self.apply_failure(&mut state, identity.clone(), attempt, failure);
        }
    }

    fn apply_failure(
        &self,
        state: &mut RestartCircuitState,
        identity: RestartIdentity,
        attempt: u64,
        failure: AttemptFailure,
    ) {
        state.clock = state.clock.wrapping_add(1);
        let touched = state.clock;
        let entry = state.budgets.entry(identity).or_insert(FailureBudget {
            failures: 0,
            latest_attempt: 0,
            touched,
        });
        // 监督事实只向前推进；旧代次迟到故障不能清掉或重复累加更新代次的预算
        if attempt <= entry.latest_attempt {
            return;
        }
        // 稳定运行过的 incarnation 先清掉旧预算，本次退出作为新周期的第一次故障
        if failure.uptime >= self.config.stability_window {
            entry.failures = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
        entry.latest_attempt = attempt;
        entry.touched = touched;
        while state.budgets.len() > MAXIMUM_TRACKED_IDENTITIES {
            let Some(identity) = state
                .budgets
                .iter()
                .filter(|(identity, _)| !state.attempts.contains_key(*identity))
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(identity, _)| identity.clone())
            else {
                break;
            };
            state.budgets.remove(&identity);
        }
    }
}

fn is_open(
    state: &RestartCircuitState,
    identity: &RestartIdentity,
    maximum_failures: NonZeroUsize,
) -> bool {
    state
        .budgets
        .get(identity)
        .is_some_and(|entry| entry.failures >= maximum_failures.get())
}
