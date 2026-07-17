//! Codex WebSocket 连接池。

mod lease;
mod state;
mod supervisor;

use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard, atomic::AtomicBool},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{sync::Semaphore, time::Instant};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

use self::state::{
    WebSocketPoolConnecting, WebSocketPoolSlot, WebSocketPoolState, account_slot_count,
    close_pooled_connection, close_pooled_connections, take_lru_idle_connection,
};
use super::pump::PumpKeepalive;

pub use self::state::CodexWebSocketPoolKey;
pub(crate) use self::{
    lease::{
        WebSocketPoolAcquire, WebSocketPoolConnectLease, WebSocketPoolConnectOutcome,
        WebSocketPoolConnectWaiter, WebSocketPoolLease,
    },
    state::{
        CodexWebSocketConnectionMetadata, PooledWebSocketConnection, WebSocketContinuationState,
    },
};

const DEFAULT_MAX_PER_ACCOUNT: usize = 8;
const DEFAULT_MAX_TOTAL: usize = 64;
const DEFAULT_MAX_CONNECTING: usize = 16;
const DEFAULT_MAX_AGE: Duration = Duration::from_mins(55);
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const DEFAULT_INITIAL_EVENT_TIMEOUT: Duration = Duration::from_secs(20);

/// WebSocket 连接池。
#[derive(Clone)]
pub struct CodexWebSocketPool {
    inner: Arc<Mutex<WebSocketPoolState>>,
    config: CodexWebSocketPoolConfig,
    tasks: TaskTracker,
    shutdown: CancellationToken,
    connect_semaphore: Arc<Semaphore>,
    maintenance_started: Arc<AtomicBool>,
}

impl Default for CodexWebSocketPool {
    fn default() -> Self {
        Self::with_config(CodexWebSocketPoolConfig::default())
    }
}

/// WebSocket 连接池配置。
#[derive(Debug, Clone, Copy)]
pub struct CodexWebSocketPoolConfig {
    /// 是否启用连接池。
    pub enabled: bool,
    /// 单个 socket 的最大生命周期。
    pub max_age: Duration,
    /// 单个账号允许占用的最大池 slot 数。
    pub max_per_account: usize,
    /// 所有账号合计允许占用的最大池 slot 数。
    pub max_total: usize,
    /// 所有账号合计允许并发执行的 opening 数。
    pub max_connecting: usize,
    /// 后台维护间隔；`None` 表示不启动后台任务。
    pub maintenance_interval: Option<Duration>,
    /// idle socket 探活 ping 间隔；`None` 表示维护时只做过期清理。
    pub ping_interval: Option<Duration>,
    /// 发送 ping 后等待上游响应的超时时间；零值表示不校验 Pong deadline。
    pub ping_timeout: Duration,
    /// idle socket 无活动多久后视为失活。
    pub liveness_timeout: Option<Duration>,
    /// 建连并发送后首个上游事件到达前的超时；`None` 表示禁用。
    pub initial_event_timeout: Option<Duration>,
}

impl Default for CodexWebSocketPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age: DEFAULT_MAX_AGE,
            max_per_account: DEFAULT_MAX_PER_ACCOUNT,
            max_total: DEFAULT_MAX_TOTAL,
            max_connecting: DEFAULT_MAX_CONNECTING,
            maintenance_interval: Some(DEFAULT_MAINTENANCE_INTERVAL),
            ping_interval: Some(DEFAULT_PING_INTERVAL),
            ping_timeout: DEFAULT_PING_TIMEOUT,
            // idle 连接不设失活截断：靠 ping/pong 保活，只在 max_age（55 分钟）
            // 或 ping 失败时关闭，最大化跨轮复用（对齐 Codex CLI 的长连接策略）。
            liveness_timeout: None,
            initial_event_timeout: Some(DEFAULT_INITIAL_EVENT_TIMEOUT),
        }
    }
}

impl CodexWebSocketPoolConfig {
    /// pump 后台任务的保活策略：从连接池配置派生出 ping/pong 与 liveness 策略。
    pub(crate) fn keepalive(&self) -> PumpKeepalive {
        PumpKeepalive {
            ping_interval: self.ping_interval,
            ping_timeout: (!self.ping_timeout.is_zero()).then_some(self.ping_timeout),
            liveness_timeout: self.liveness_timeout,
        }
    }
}

impl CodexWebSocketPool {
    /// 构造连接池策略和状态。
    pub fn new(max_per_account: usize, max_age: Duration) -> Self {
        Self::with_config(CodexWebSocketPoolConfig {
            max_per_account: max_per_account.max(1),
            max_age,
            maintenance_interval: None,
            ping_interval: None,
            liveness_timeout: None,
            ..CodexWebSocketPoolConfig::default()
        })
    }

    /// 使用完整配置构造连接池。
    pub fn with_config(config: CodexWebSocketPoolConfig) -> Self {
        let pool = Self {
            inner: Arc::new(Mutex::new(WebSocketPoolState::default())),
            config,
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            connect_semaphore: Arc::new(Semaphore::new(config.max_connecting)),
            maintenance_started: Arc::new(AtomicBool::new(false)),
        };
        pool.spawn_maintenance_task();
        pool
    }

    /// pump 后台任务的保活策略（供建连时传入）。
    pub(crate) fn keepalive(&self) -> PumpKeepalive {
        self.config.keepalive()
    }

    /// 建连并发送后首个上游事件到达前的超时；`None` 表示禁用。
    pub(crate) fn initial_event_timeout(&self) -> Option<Duration> {
        self.config.initial_event_timeout
    }

    /// 注册由连接池生命周期托管的 opening 任务。
    pub(crate) fn spawn_connect_task(&self, future: impl Future<Output = ()> + Send + 'static) {
        drop(self.tasks.spawn(future));
    }

    pub(crate) async fn acquire(
        &self,
        key: &CodexWebSocketPoolKey,
        required_response_id: Option<&str>,
    ) -> WebSocketPoolAcquire {
        self.spawn_maintenance_task();
        let mut connections_to_close = Vec::new();
        let acquire = {
            let mut state = self.lock_state();
            if !self.config.enabled || state.shutting_down {
                return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Disabled);
            }
            let key = if let Some(response_id) = required_response_id {
                let Some(key) = state.slots.iter().find_map(|(candidate, slot)| {
                    (candidate.same_logical_connection(key)
                        && slot.latest_response_id() == Some(response_id))
                    .then(|| candidate.clone())
                }) else {
                    return WebSocketPoolAcquire::Bypass(
                        WebSocketPoolBypassReason::ContinuationNotFound,
                    );
                };
                key
            } else {
                key.clone()
            };
            match state.slots.get(&key) {
                Some(WebSocketPoolSlot::Busy(_)) => {
                    return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                }
                Some(WebSocketPoolSlot::Connecting(connecting)) => {
                    return WebSocketPoolAcquire::Wait(WebSocketPoolConnectWaiter {
                        receiver: connecting.outcome.subscribe(),
                        started_at: connecting.started_at,
                    });
                }
                Some(WebSocketPoolSlot::Idle { .. }) => {
                    let Some(WebSocketPoolSlot::Idle { connection, .. }) = state.slots.remove(&key)
                    else {
                        return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                    };
                    // 零成本探活：后台 pump 已实时感知连接死亡（RST/Close/EOF/失活），
                    // 复用前只需读取 is_closed 标志，避免复用到静默死连接后卡到超时。
                    if connection.created_at.elapsed() < self.config.max_age
                        && !connection.websocket.is_closed()
                    {
                        let lease = WebSocketPoolLease::reserve(
                            self.clone(),
                            key.clone(),
                            connection.continuation.latest_response_id(),
                        );
                        state.slots.insert(
                            key.clone(),
                            WebSocketPoolSlot::Busy(lease.reservation.clone()),
                        );
                        return WebSocketPoolAcquire::Reused { connection, lease };
                    }
                    connections_to_close.push(*connection);
                }
                None => {}
            }

            let acquire = if required_response_id.is_some() {
                WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::ContinuationNotFound)
            } else if self.config.max_per_account == 0
                || account_slot_count(&state.slots, key.account_id()) >= self.config.max_per_account
            {
                WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Cap)
            } else {
                let connect_permit = self.connect_semaphore.clone().try_acquire_owned().ok();
                match connect_permit {
                    Some(connect_permit) => {
                        let has_total_capacity = if self.config.max_total == 0 {
                            false
                        } else if state.slots.len() < self.config.max_total {
                            true
                        } else if let Some(connection) = take_lru_idle_connection(&mut state) {
                            connections_to_close.push(connection);
                            true
                        } else {
                            false
                        };
                        if has_total_capacity {
                            let lease = WebSocketPoolConnectLease::reserve(
                                self.clone(),
                                key.clone(),
                                connect_permit,
                            );
                            state.slots.insert(
                                key,
                                WebSocketPoolSlot::Connecting(WebSocketPoolConnecting {
                                    id: lease.id,
                                    started_at: lease.started_at,
                                    outcome: lease.outcome.clone(),
                                    cancellation: lease.cancellation.clone(),
                                }),
                            );
                            WebSocketPoolAcquire::Connect(lease)
                        } else {
                            WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Cap)
                        }
                    }
                    None => WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Cap),
                }
            };
            drop(state);
            acquire
        };

        close_pooled_connections(connections_to_close).await;

        acquire
    }

    async fn put_reserved(
        &self,
        key: &CodexWebSocketPoolKey,
        reservation_id: Uuid,
        connection: PooledWebSocketConnection,
    ) {
        let mut connection = Some(connection);
        {
            let mut state = self.lock_state();
            let expired = connection
                .as_ref()
                .is_some_and(|connection| connection.created_at.elapsed() >= self.config.max_age);
            let owns_reservation = matches!(
                state.slots.get(key),
                Some(WebSocketPoolSlot::Busy(reservation))
                    if reservation.id == reservation_id
            );
            if owns_reservation && (expired || state.shutting_down || !self.config.enabled) {
                state.slots.remove(key);
            } else if owns_reservation && let Some(connection) = connection.take() {
                state.slots.insert(
                    key.clone(),
                    WebSocketPoolSlot::Idle {
                        connection: Box::new(connection),
                        last_used_at: Instant::now(),
                    },
                );
            }
        }
        if let Some(connection) = connection {
            close_pooled_connection(connection).await;
        }
    }

    async fn discard_reserved(&self, key: &CodexWebSocketPoolKey, reservation_id: Uuid) {
        self.discard_reserved_now(key, reservation_id);
    }

    fn discard_reserved_now(&self, key: &CodexWebSocketPoolKey, reservation_id: Uuid) {
        let mut state = self.lock_state();
        if matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Busy(reservation)) if reservation.id == reservation_id
        ) {
            state.slots.remove(key);
        }
    }

    async fn finish_connect_reserved(
        &self,
        key: &CodexWebSocketPoolKey,
        connect_id: Uuid,
        connection: PooledWebSocketConnection,
    ) -> Result<(Box<PooledWebSocketConnection>, WebSocketPoolLease), Box<PooledWebSocketConnection>>
    {
        let mut state = self.lock_state();
        let owns_connect = matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Connecting(connecting)) if connecting.id == connect_id
        );
        if owns_connect && !state.shutting_down && self.config.enabled {
            let lease = WebSocketPoolLease::reserve(self.clone(), key.clone(), None);
            state.slots.insert(
                key.clone(),
                WebSocketPoolSlot::Busy(lease.reservation.clone()),
            );
            Ok((Box::new(connection), lease))
        } else {
            if owns_connect {
                state.slots.remove(key);
            }
            Err(Box::new(connection))
        }
    }

    async fn fail_connect(&self, key: &CodexWebSocketPoolKey, connect_id: Uuid) {
        self.fail_connect_now(key, connect_id);
    }

    fn fail_connect_now(&self, key: &CodexWebSocketPoolKey, connect_id: Uuid) {
        let mut state = self.lock_state();
        if matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Connecting(connecting)) if connecting.id == connect_id
        ) {
            state.slots.remove(key);
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, WebSocketPoolState> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketPoolBypassReason {
    Disabled,
    Busy,
    Cap,
    ContinuationNotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSocketPoolDecision {
    kind: WebSocketPoolDecisionKind,
}

impl WebSocketPoolDecision {
    pub fn new() -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::New,
        }
    }

    pub fn reuse() -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::Reuse,
        }
    }

    pub fn kind(self) -> &'static str {
        self.kind.as_str()
    }

    pub fn metadata_value(self) -> Value {
        json!({ "kind": self.kind() })
    }
}

impl Default for WebSocketPoolDecision {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSocketPoolDecisionKind {
    New,
    Reuse,
}

impl WebSocketPoolDecisionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reuse => "reuse",
        }
    }
}
