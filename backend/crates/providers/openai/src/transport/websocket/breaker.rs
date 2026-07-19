//! Responses WebSocket 冷建连的 origin 级熔断器。

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use uuid::Uuid;

const DEFAULT_FAILURE_THRESHOLD: usize = 3;
const DEFAULT_FAILURE_WINDOW: Duration = Duration::from_secs(30);
const DEFAULT_OPEN_DURATION: Duration = Duration::from_secs(30);

/// origin 熔断策略。
#[derive(Debug, Clone, Copy)]
pub struct WebSocketOriginBreakerConfig {
    pub failure_threshold: usize,
    pub failure_window: Duration,
    pub open_duration: Duration,
}

impl Default for WebSocketOriginBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            failure_window: DEFAULT_FAILURE_WINDOW,
            open_duration: DEFAULT_OPEN_DURATION,
        }
    }
}

/// 冷建连是否获准进入 origin。
pub enum WebSocketOriginBreakerDecision {
    Allowed(WebSocketOriginBreakerPermit),
    Open,
    HalfOpenBusy,
}

/// origin 级 WebSocket 快路径熔断器。
#[derive(Clone)]
pub struct WebSocketOriginBreaker {
    inner: Arc<Mutex<HashMap<String, CircuitState>>>,
    config: WebSocketOriginBreakerConfig,
}

impl Default for WebSocketOriginBreaker {
    fn default() -> Self {
        Self::with_config(WebSocketOriginBreakerConfig::default())
    }
}

impl WebSocketOriginBreaker {
    pub fn with_config(config: WebSocketOriginBreakerConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            config: WebSocketOriginBreakerConfig {
                failure_threshold: config.failure_threshold.max(1),
                ..config
            },
        }
    }

    /// 已有热 socket 不经过这里；本方法只裁决新的 WebSocket opening。
    pub fn try_acquire(&self, origin_key: &str) -> WebSocketOriginBreakerDecision {
        let now = Instant::now();
        let mut circuits = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = circuits
            .entry(origin_key.to_string())
            .or_insert_with(CircuitState::closed);

        match state {
            CircuitState::Closed { failures } => {
                retain_window(failures, now, self.config.failure_window);
                WebSocketOriginBreakerDecision::Allowed(WebSocketOriginBreakerPermit::new(
                    self.clone(),
                    origin_key.to_string(),
                    None,
                ))
            }
            CircuitState::Open { until } if now < *until => WebSocketOriginBreakerDecision::Open,
            CircuitState::Open { .. } => {
                let probe_id = Uuid::new_v4();
                *state = CircuitState::HalfOpen { probe_id };
                WebSocketOriginBreakerDecision::Allowed(WebSocketOriginBreakerPermit::new(
                    self.clone(),
                    origin_key.to_string(),
                    Some(probe_id),
                ))
            }
            CircuitState::HalfOpen { .. } => WebSocketOriginBreakerDecision::HalfOpenBusy,
        }
    }

    fn record_success(&self, origin_key: &str, probe_id: Option<Uuid>) {
        let mut circuits = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if permit_still_owns_state(circuits.get(origin_key), probe_id) {
            circuits.insert(origin_key.to_string(), CircuitState::closed());
        }
    }

    fn record_failure(&self, origin_key: &str, probe_id: Option<Uuid>) {
        let now = Instant::now();
        let mut circuits = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !permit_still_owns_state(circuits.get(origin_key), probe_id) {
            return;
        }
        if probe_id.is_some() {
            circuits.insert(
                origin_key.to_string(),
                CircuitState::Open {
                    until: now + self.config.open_duration,
                },
            );
            return;
        }

        let state = circuits
            .entry(origin_key.to_string())
            .or_insert_with(CircuitState::closed);
        let CircuitState::Closed { failures } = state else {
            return;
        };
        retain_window(failures, now, self.config.failure_window);
        failures.push_back(now);
        if failures.len() >= self.config.failure_threshold {
            *state = CircuitState::Open {
                until: now + self.config.open_duration,
            };
        }
    }

    fn cancel_probe(&self, origin_key: &str, probe_id: Option<Uuid>) {
        let Some(probe_id) = probe_id else {
            return;
        };
        let mut circuits = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if permit_still_owns_state(circuits.get(origin_key), Some(probe_id)) {
            circuits.insert(
                origin_key.to_string(),
                CircuitState::Open {
                    until: Instant::now(),
                },
            );
        }
    }
}

/// 一次新的 WebSocket opening 许可；消费式完成保证 half-open 探针不会泄漏。
pub struct WebSocketOriginBreakerPermit {
    breaker: WebSocketOriginBreaker,
    origin_key: String,
    probe_id: Option<Uuid>,
    degradation_recorded: Arc<AtomicBool>,
    armed: bool,
}

impl WebSocketOriginBreakerPermit {
    fn new(breaker: WebSocketOriginBreaker, origin_key: String, probe_id: Option<Uuid>) -> Self {
        Self {
            breaker,
            origin_key,
            probe_id,
            degradation_recorded: Arc::new(AtomicBool::new(false)),
            armed: true,
        }
    }

    pub fn is_half_open_probe(&self) -> bool {
        self.probe_id.is_some()
    }

    /// 创建可跨前台预算与后台 opening 生命周期共享的快路径观察器。
    pub(crate) fn fast_path_reporter(&self) -> WebSocketOriginFastPathReporter {
        WebSocketOriginFastPathReporter {
            breaker: self.breaker.clone(),
            origin_key: self.origin_key.clone(),
            probe_id: self.probe_id,
            degradation_recorded: Arc::clone(&self.degradation_recorded),
        }
    }

    pub fn succeed(mut self) {
        if !self.degradation_recorded.load(Ordering::Acquire) {
            self.breaker.record_success(&self.origin_key, self.probe_id);
        }
        self.armed = false;
    }

    pub fn fast_timeout(mut self) {
        self.fast_path_reporter().missed();
        self.armed = false;
    }

    pub fn fail(mut self) {
        self.record_failure_once();
        self.armed = false;
    }

    /// opening 因账号驱逐或服务关闭而取消，不污染 origin 可用性。
    pub(crate) fn cancel(mut self) {
        self.breaker.cancel_probe(&self.origin_key, self.probe_id);
        self.armed = false;
    }

    fn record_failure_once(&self) {
        if !self.degradation_recorded.swap(true, Ordering::AcqRel) {
            self.breaker.record_failure(&self.origin_key, self.probe_id);
        }
    }
}

impl Drop for WebSocketOriginBreakerPermit {
    fn drop(&mut self) {
        if self.armed {
            self.record_failure_once();
        }
    }
}

/// 前台等待结束后仍可报告同一次后台 opening 错过快路径预算。
pub(crate) struct WebSocketOriginFastPathReporter {
    breaker: WebSocketOriginBreaker,
    origin_key: String,
    probe_id: Option<Uuid>,
    degradation_recorded: Arc<AtomicBool>,
}

impl WebSocketOriginFastPathReporter {
    pub(crate) fn missed(&self) {
        if !self.degradation_recorded.swap(true, Ordering::AcqRel) {
            self.breaker.record_failure(&self.origin_key, self.probe_id);
        }
    }
}

enum CircuitState {
    Closed { failures: VecDeque<Instant> },
    Open { until: Instant },
    HalfOpen { probe_id: Uuid },
}

impl CircuitState {
    fn closed() -> Self {
        Self::Closed {
            failures: VecDeque::new(),
        }
    }
}

fn retain_window(timeouts: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while timeouts
        .front()
        .is_some_and(|recorded_at| now.duration_since(*recorded_at) > window)
    {
        timeouts.pop_front();
    }
}

fn permit_still_owns_state(state: Option<&CircuitState>, probe_id: Option<Uuid>) -> bool {
    match (state, probe_id) {
        (Some(CircuitState::Closed { .. }), None) => true,
        (Some(CircuitState::HalfOpen { probe_id: active }), Some(expected)) => *active == expected,
        _ => false,
    }
}
