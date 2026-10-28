//! Codex WebSocket 连接池

mod capacity;
mod downstream;
mod lease;
mod state;
mod supervisor;

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, MutexGuard, Weak, atomic::AtomicBool},
    time::Duration,
};

use tokio::sync::{Notify, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

use self::state::{
    WebSocketPoolConnecting, WebSocketPoolSlot, WebSocketPoolState, close_pooled_connection,
    close_pooled_connections, idle_connection_loss,
};
use super::pump::WebSocketConnectionObservation;

use self::capacity::ProxyCapacity;
pub(crate) use self::capacity::{ProxyConnectionPermit, proxy_capacity_key};
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

const DEFAULT_MAX_CONNECTING: usize = 16;
const DEFAULT_MAX_AGE: Duration = Duration::from_mins(55);
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// WebSocket 连接池
#[derive(Clone)]
pub struct CodexWebSocketPool {
    inner: Arc<Mutex<WebSocketPoolState>>,
    config: CodexWebSocketPoolConfig,
    tasks: TaskTracker,
    shutdown: CancellationToken,
    connect_semaphore: Arc<Semaphore>,
    maintenance_started: Arc<AtomicBool>,
    proxy_capacities: Arc<Mutex<HashMap<String, Weak<ProxyCapacity>>>>,
    capacity_changed: Arc<Notify>,
}

impl Default for CodexWebSocketPool {
    fn default() -> Self {
        Self::with_config(CodexWebSocketPoolConfig::default())
    }
}

/// WebSocket 连接池配置
#[derive(Debug, Clone, Copy)]
pub struct CodexWebSocketPoolConfig {
    /// 是否启用连接池
    pub enabled: bool,
    /// 单个 socket 的最大生命周期
    pub max_age: Duration,
    /// 所有账号合计允许并发执行的 opening 数
    pub max_connecting: usize,
    /// 后台维护间隔；`None` 表示不启动后台任务
    pub maintenance_interval: Option<Duration>,
    /// 未绑定下游 WS 的连接归池后可空闲的时间，不由 Ping/Pong 刷新
    pub idle_timeout: Duration,
    /// 等待下一条上游消息的空闲超时；`None` 或零值使用默认 300 秒
    pub stream_idle_timeout: Option<Duration>,
}

impl Default for CodexWebSocketPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age: DEFAULT_MAX_AGE,
            max_connecting: DEFAULT_MAX_CONNECTING,
            maintenance_interval: Some(DEFAULT_MAINTENANCE_INTERVAL),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            stream_idle_timeout: Some(DEFAULT_STREAM_IDLE_TIMEOUT),
        }
    }
}

impl CodexWebSocketPool {
    /// 使用指定连接总寿命构造连接池，不启动定时清理
    pub fn new(max_age: Duration) -> Self {
        Self::with_config(CodexWebSocketPoolConfig {
            max_age,
            maintenance_interval: None,
            ..CodexWebSocketPoolConfig::default()
        })
    }

    /// 使用完整配置构造连接池
    pub fn with_config(config: CodexWebSocketPoolConfig) -> Self {
        let pool = Self {
            inner: Arc::new(Mutex::new(WebSocketPoolState::default())),
            config,
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            connect_semaphore: Arc::new(Semaphore::new(config.max_connecting)),
            maintenance_started: Arc::new(AtomicBool::new(false)),
            proxy_capacities: Arc::default(),
            capacity_changed: Arc::default(),
        };
        pool.spawn_maintenance_task();
        pool
    }

    /// 等待下一条上游消息的空闲超时；`None` 或零值使用默认 300 秒
    pub(crate) fn stream_idle_timeout(&self) -> Option<Duration> {
        self.config.stream_idle_timeout
    }

    /// 注册由连接池生命周期托管的 opening 任务
    pub(crate) fn spawn_connect_task(&self, future: impl Future<Output = ()> + Send + 'static) {
        drop(self.tasks.spawn(future));
    }

    pub(crate) async fn acquire(
        &self,
        key: &CodexWebSocketPoolKey,
        required_response_id: Option<&str>,
        proxy: Option<&gateway_core::account::OutboundProxy>,
        downstream_cancellation: Option<&gateway_core::lifecycle::CancellationToken>,
    ) -> WebSocketPoolAcquire {
        self.spawn_maintenance_task();
        if let Some(cancellation) = downstream_cancellation {
            self.register_downstream(key, cancellation);
        }
        let mut proxy_permit = None;
        loop {
            let mut connections_to_close = Vec::new();
            let acquire = {
                let mut state = self.lock_state();
                let mut continuation_loss = None;
                if !self.config.enabled
                    || state.shutting_down
                    || downstream_cancellation
                        .is_some_and(gateway_core::lifecycle::CancellationToken::is_cancelled)
                {
                    return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Disabled);
                }
                let requested_key = key;
                let key = if let Some(response_id) = required_response_id {
                    let Some(key) = state.slots.iter().find_map(|(candidate, slot)| {
                        (candidate.same_logical_connection(key)
                            && slot.latest_response_id() == Some(response_id))
                        .then(|| candidate.clone())
                    }) else {
                        return state
                            .continuation_loss(key, response_id, tokio::time::Instant::now())
                            .map_or(
                                WebSocketPoolAcquire::Bypass(
                                    WebSocketPoolBypassReason::ContinuationNotFound,
                                ),
                                WebSocketPoolAcquire::ContinuationLost,
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
                        let Some(WebSocketPoolSlot::Idle {
                            mut connection,
                            idle_since,
                        }) = state.slots.remove(&key)
                        else {
                            return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                        };
                        // 取得时与后台扫描共用到期判断，避免维护周期内重新借出过期连接
                        let loss = idle_connection_loss(
                            state.has_downstream_binding(&key),
                            &connection,
                            idle_since,
                            tokio::time::Instant::now(),
                            &self.config,
                        )
                        .or_else(|| {
                            state.downstream_closed(&key).then(|| {
                                connection
                                    .websocket
                                    .observation()
                                    .with_exit_reason("downstream_connection_closed")
                            })
                        });
                        let Some(observation) = loss else {
                            connection.websocket.revoke_control();
                            let retired = state.take_downstream_connections(
                                &requested_key.downstream_connection_id,
                                Some(&key),
                                "downstream_rebound",
                            );
                            // 精确续接可接手另一条下游连接的 socket，池键必须跟随新 owner
                            let key = key.with_downstream_connection_id(
                                requested_key.downstream_connection_id.clone(),
                            );
                            let lease = WebSocketPoolLease::reserve(
                                self.clone(),
                                key.clone(),
                                connection.continuation.latest_response_id(),
                            );
                            state
                                .slots
                                .insert(key, WebSocketPoolSlot::Busy(lease.reservation.clone()));
                            self.close_retired_connections(retired);
                            return WebSocketPoolAcquire::Reused { connection, lease };
                        };
                        tracing::info!(
                            websocket_connection_id = %connection.websocket.connection_id(),
                            exit_reason = observation.exit_reason(),
                            idle_age_ms = idle_since.elapsed().as_millis(),
                            "Removed unavailable idle WebSocket before reuse"
                        );
                        state.remember_continuation_loss(
                            &key,
                            connection.continuation.latest_response_id(),
                            observation.clone(),
                            tokio::time::Instant::now(),
                        );
                        connections_to_close.push(*connection);
                        if required_response_id.is_some() {
                            continuation_loss = Some(observation);
                        }
                    }
                    None => {}
                }

                connections_to_close.extend(state.take_downstream_connections(
                    &requested_key.downstream_connection_id,
                    Some(&key),
                    "downstream_rebound",
                ));
                let acquire = if let Some(observation) = continuation_loss {
                    Some(WebSocketPoolAcquire::ContinuationLost(observation))
                } else if required_response_id.is_some() {
                    Some(WebSocketPoolAcquire::Bypass(
                        WebSocketPoolBypassReason::ContinuationNotFound,
                    ))
                } else if proxy.is_some() && proxy_permit.is_none() {
                    None
                } else {
                    let connect_permit = self.connect_semaphore.clone().try_acquire_owned().ok();
                    Some(match connect_permit {
                        Some(connect_permit) => {
                            let lease = WebSocketPoolConnectLease::reserve(
                                self.clone(),
                                key.clone(),
                                connect_permit,
                                proxy_permit.take(),
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
                        }
                        None => WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Cap),
                    })
                };
                drop(state);
                acquire
            };

            close_pooled_connections(connections_to_close).await;

            if let Some(acquire) = acquire {
                return acquire;
            }
            let capacity = tokio::select! {
                biased;
                () = async {
                    match downstream_cancellation {
                        Some(cancellation) => cancellation.cancelled().await,
                        None => std::future::pending().await,
                    }
                } => return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Disabled),
                capacity = self.acquire_proxy_capacity(proxy, Some(key)) => capacity,
            };
            match capacity {
                Ok(permit) => proxy_permit = permit,
                Err(_) => return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Disabled),
            }
        }
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
                if expired && let Some(connection) = connection.as_ref() {
                    state.remember_continuation_loss(
                        key,
                        connection.continuation.latest_response_id(),
                        connection
                            .websocket
                            .observation()
                            .with_exit_reason("max_age_expired"),
                        tokio::time::Instant::now(),
                    );
                }
                state.slots.remove(key);
            } else if owns_reservation && let Some(connection) = connection.take() {
                state.slots.insert(
                    key.clone(),
                    WebSocketPoolSlot::Idle {
                        connection: Box::new(connection),
                        idle_since: tokio::time::Instant::now(),
                    },
                );
            }
        }
        if let Some(connection) = connection {
            close_pooled_connection(connection).await;
        }
        self.capacity_changed.notify_waiters();
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

    fn discard_reserved_with_observation(
        &self,
        key: &CodexWebSocketPoolKey,
        reservation_id: Uuid,
        observation: WebSocketConnectionObservation,
    ) {
        let mut state = self.lock_state();
        let latest_response_id = match state.slots.get(key) {
            Some(WebSocketPoolSlot::Busy(reservation)) if reservation.id == reservation_id => {
                reservation.latest_response_id.clone()
            }
            _ => return,
        };
        state.remember_continuation_loss(
            key,
            latest_response_id.as_deref(),
            observation,
            tokio::time::Instant::now(),
        );
        state.slots.remove(key);
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

    pub const fn is_reuse(self) -> bool {
        matches!(self.kind, WebSocketPoolDecisionKind::Reuse)
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
