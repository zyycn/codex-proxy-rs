//! Codex WebSocket 连接池。

use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    sync::{Mutex, watch},
    time::Instant,
};
use tokio_util::{
    sync::CancellationToken,
    task::{TaskTracker, task_tracker::TaskTrackerToken},
};
use uuid::Uuid;

use super::websocket_pump::{PumpKeepalive, PumpedWebSocket};

const DEFAULT_MAX_PER_ACCOUNT: usize = 8;
const DEFAULT_MAX_AGE: Duration = Duration::from_mins(55);
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const DEFAULT_INITIAL_EVENT_TIMEOUT: Duration = Duration::from_secs(20);

/// WebSocket 连接池 key。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodexWebSocketPoolKey {
    base_url: String,
    account_id: String,
    conversation_id: String,
}

impl CodexWebSocketPoolKey {
    /// 构造连接池 key。
    pub fn new(
        base_url: impl Into<String>,
        account_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            account_id: account_id.into(),
            conversation_id: conversation_id.into(),
        }
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }

    pub(crate) fn conversation_id_hash(&self) -> String {
        short_sha256([self.conversation_id.as_str()])
    }

    pub(crate) fn stable_hash(&self) -> String {
        short_sha256([
            self.base_url.as_str(),
            self.account_id.as_str(),
            self.conversation_id.as_str(),
        ])
    }
}

/// WebSocket 连接池。
#[derive(Clone)]
pub struct CodexWebSocketPool {
    inner: Arc<Mutex<WebSocketPoolState>>,
    config: CodexWebSocketPoolConfig,
    tasks: TaskTracker,
    shutdown: CancellationToken,
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

    pub(crate) async fn acquire(&self, key: &CodexWebSocketPoolKey) -> WebSocketPoolAcquire {
        let mut expired_connection = None;
        let acquire = {
            let mut state = self.inner.lock().await;
            if !self.config.enabled || state.shutting_down {
                return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Disabled);
            }
            match state.slots.get(key) {
                Some(WebSocketPoolSlot::Busy(_)) => {
                    return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                }
                Some(WebSocketPoolSlot::Connecting(connecting)) => {
                    return WebSocketPoolAcquire::Wait(WebSocketPoolConnectWaiter {
                        receiver: connecting.outcome.subscribe(),
                        started_at: connecting.started_at,
                    });
                }
                Some(WebSocketPoolSlot::Idle(_)) => {
                    let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(key) else {
                        return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                    };
                    // 零成本探活：后台 pump 已实时感知连接死亡（RST/Close/EOF/失活），
                    // 复用前只需读取 is_closed 标志，避免复用到静默死连接后卡到超时。
                    if connection.created_at.elapsed() < self.config.max_age
                        && !connection.websocket.is_closed()
                    {
                        let lease = WebSocketPoolLease::reserve(self.clone(), key.clone());
                        state
                            .slots
                            .insert(key.clone(), WebSocketPoolSlot::Busy(lease.reservation));
                        return WebSocketPoolAcquire::Reused { connection, lease };
                    }
                    expired_connection = Some(*connection);
                }
                None => {}
            }

            let acquire = if self.config.max_per_account == 0
                || account_slot_count(&state.slots, key.account_id()) >= self.config.max_per_account
            {
                WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Cap)
            } else {
                let lease = WebSocketPoolConnectLease::reserve(self.clone(), key.clone());
                state.slots.insert(
                    key.clone(),
                    WebSocketPoolSlot::Connecting(WebSocketPoolConnecting {
                        id: lease.id,
                        started_at: lease.started_at,
                        outcome: lease.outcome.clone(),
                        cancellation: lease.cancellation.clone(),
                    }),
                );
                WebSocketPoolAcquire::Connect(lease)
            };
            drop(state);
            acquire
        };

        if let Some(connection) = expired_connection {
            close_pooled_connection(connection).await;
        }

        acquire
    }

    /// 只租用已经处于 Idle 的连接，不创建新的 Connecting slot。
    pub(crate) async fn take_idle(
        &self,
        key: &CodexWebSocketPoolKey,
    ) -> Option<(Box<PooledWebSocketConnection>, WebSocketPoolLease)> {
        let mut expired_connection = None;
        let acquired = {
            let mut state = self.inner.lock().await;
            if !self.config.enabled || state.shutting_down {
                return None;
            }
            let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(key) else {
                return None;
            };
            if connection.created_at.elapsed() < self.config.max_age
                && !connection.websocket.is_closed()
            {
                let lease = WebSocketPoolLease::reserve(self.clone(), key.clone());
                state
                    .slots
                    .insert(key.clone(), WebSocketPoolSlot::Busy(lease.reservation));
                Some((connection, lease))
            } else {
                expired_connection = Some(*connection);
                None
            }
        };
        if let Some(connection) = expired_connection {
            close_pooled_connection(connection).await;
        }
        acquired
    }

    async fn put_reserved(
        &self,
        key: &CodexWebSocketPoolKey,
        reservation_id: Uuid,
        connection: PooledWebSocketConnection,
    ) {
        let mut connection = Some(connection);
        {
            let mut state = self.inner.lock().await;
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
                state
                    .slots
                    .insert(key.clone(), WebSocketPoolSlot::Idle(Box::new(connection)));
            }
        }
        if let Some(connection) = connection {
            close_pooled_connection(connection).await;
        }
    }

    async fn discard_reserved(&self, key: &CodexWebSocketPoolKey, reservation_id: Uuid) {
        let mut state = self.inner.lock().await;
        if matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Busy(reservation)) if reservation.id == reservation_id
        ) {
            state.slots.remove(key);
        }
    }

    async fn finish_connect_idle(
        &self,
        key: &CodexWebSocketPoolKey,
        connect_id: Uuid,
        connection: PooledWebSocketConnection,
    ) -> Result<(), Box<PooledWebSocketConnection>> {
        let mut state = self.inner.lock().await;
        let owns_connect = matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Connecting(connecting)) if connecting.id == connect_id
        );
        if owns_connect && !state.shutting_down && self.config.enabled {
            state
                .slots
                .insert(key.clone(), WebSocketPoolSlot::Idle(Box::new(connection)));
            Ok(())
        } else {
            if owns_connect {
                state.slots.remove(key);
            }
            Err(Box::new(connection))
        }
    }

    async fn fail_connect(&self, key: &CodexWebSocketPoolKey, connect_id: Uuid) {
        let mut state = self.inner.lock().await;
        if matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Connecting(connecting)) if connecting.id == connect_id
        ) {
            state.slots.remove(key);
        }
    }

    /// 驱逐指定账号的全部 slot，取消 opening，并阻止 busy 连接回收到池中。
    pub async fn evict_account(&self, account_id: &str) {
        let mut idle_connections = Vec::new();
        {
            let mut state = self.inner.lock().await;
            let keys = state
                .slots
                .keys()
                .filter(|key| key.account_id() == account_id)
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                match state.slots.remove(&key) {
                    Some(WebSocketPoolSlot::Idle(connection)) => {
                        idle_connections.push(*connection);
                    }
                    Some(WebSocketPoolSlot::Connecting(connecting)) => {
                        connecting.cancellation.cancel();
                        connecting
                            .outcome
                            .send_replace(WebSocketPoolConnectOutcome::Failed);
                    }
                    Some(WebSocketPoolSlot::Busy(_)) | None => {}
                }
            }
        }
        close_pooled_connections(idle_connections).await;
    }

    /// 关闭连接池，取消受管任务、关闭 idle 连接，并让后续 acquire 直接绕过池。
    pub async fn shutdown(&self) {
        self.tasks.close();
        self.shutdown.cancel();
        let idle_connections = {
            let mut state = self.inner.lock().await;
            state.shutting_down = true;
            state
                .slots
                .drain()
                .filter_map(|(_, slot)| match slot {
                    WebSocketPoolSlot::Idle(connection) => Some(*connection),
                    WebSocketPoolSlot::Connecting(connecting) => {
                        connecting.cancellation.cancel();
                        connecting
                            .outcome
                            .send_replace(WebSocketPoolConnectOutcome::Failed);
                        None
                    }
                    WebSocketPoolSlot::Busy(_) => None,
                })
                .collect::<Vec<_>>()
        };
        close_pooled_connections(idle_connections).await;
        self.tasks.wait().await;
    }

    /// 维护池 slot：清扫已死亡或超龄的 idle 连接，以及异常残留的 Busy reservation。
    ///
    /// 保活（ping/pong）与失活检测已下沉到每条连接的 pump 任务内部，此处不再做
    /// 同步 ping 探活，只负责把「后台已标记 closed」或「超过 max_age」的 idle
    /// 连接从池中摘除并关闭，避免它们占用 slot。
    pub async fn maintain_idle_connections(&self) {
        let close = self.take_expired_slots().await;
        close_pooled_connections(close).await;
    }

    fn spawn_maintenance_task(&self) {
        let Some(interval_duration) = self.config.maintenance_interval else {
            return;
        };
        let Ok(_handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let inner = Arc::downgrade(&self.inner);
        let config = self.config;
        let tasks = self.tasks.clone();
        let shutdown = self.shutdown.clone();
        drop(self.tasks.spawn(async move {
            let mut interval = tokio::time::interval(interval_duration);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = interval.tick() => {}
                }
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                let pool = CodexWebSocketPool {
                    inner,
                    config,
                    tasks: tasks.clone(),
                    shutdown: shutdown.clone(),
                };
                if pool.is_shutdown().await {
                    break;
                }
                pool.maintain_idle_connections().await;
            }
        }));
    }

    /// 返回连接池是否已进入关闭状态。
    pub async fn is_shutdown(&self) -> bool {
        self.inner.lock().await.shutting_down
    }

    async fn take_expired_slots(&self) -> Vec<PooledWebSocketConnection> {
        let mut close = Vec::new();
        let now = Instant::now();
        let mut state = self.inner.lock().await;
        if state.shutting_down || !self.config.enabled {
            return close;
        }
        let keys = state
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                WebSocketPoolSlot::Idle(connection)
                    if should_close_idle_connection(connection, now, self.config.max_age) =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Busy(reservation)
                    if now.duration_since(reservation.reserved_at) >= self.config.max_age =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Connecting(connecting)
                    if now.duration_since(connecting.started_at) >= self.config.max_age =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Idle(_)
                | WebSocketPoolSlot::Busy(_)
                | WebSocketPoolSlot::Connecting(_) => None,
            })
            .collect::<Vec<_>>();
        for key in keys {
            match state.slots.remove(&key) {
                Some(WebSocketPoolSlot::Idle(connection)) => close.push(*connection),
                Some(WebSocketPoolSlot::Busy(reservation)) => {
                    tracing::warn!(
                        account_id = key.account_id(),
                        conversation_id_hash = key.conversation_id_hash(),
                        reservation_id = %reservation.id,
                        "Removed stale WebSocket pool reservation"
                    );
                }
                Some(WebSocketPoolSlot::Connecting(connecting)) => {
                    connecting.cancellation.cancel();
                    connecting
                        .outcome
                        .send_replace(WebSocketPoolConnectOutcome::Failed);
                    tracing::warn!(
                        account_id = key.account_id(),
                        conversation_id_hash = key.conversation_id_hash(),
                        connect_id = %connecting.id,
                        "Removed stale WebSocket pool connection attempt"
                    );
                }
                None => {}
            }
        }
        close
    }
}

#[derive(Default)]
struct WebSocketPoolState {
    slots: HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    shutting_down: bool,
}

#[derive(Clone)]
pub(crate) struct CodexWebSocketConnectionMetadata {
    pub(crate) turn_state: Option<String>,
    pub(crate) set_cookie_headers: Vec<String>,
    pub(crate) rate_limit_headers: Vec<(String, String)>,
    pub(crate) response_metadata: super::client::CodexResponseMetadata,
    pub(crate) diagnostics: super::diagnostics::CodexUpstreamDiagnostics,
}

pub(crate) struct PooledWebSocketConnection {
    pub(crate) websocket: PumpedWebSocket,
    pub(crate) metadata: CodexWebSocketConnectionMetadata,
    pub(crate) continuation: WebSocketContinuationState,
    pub(crate) created_at: Instant,
}

/// 只随具体 WebSocket 生命周期存在的续接状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WebSocketContinuationState {
    latest_response_id: Option<String>,
}

impl WebSocketContinuationState {
    pub(crate) fn latest_response_id(&self) -> Option<&str> {
        self.latest_response_id.as_deref()
    }

    pub(crate) fn record_completed(&mut self, response_id: String) {
        self.latest_response_id = Some(response_id);
    }
}

enum WebSocketPoolSlot {
    Idle(Box<PooledWebSocketConnection>),
    Busy(WebSocketPoolReservation),
    Connecting(WebSocketPoolConnecting),
}

pub(crate) enum WebSocketPoolAcquire {
    Reused {
        connection: Box<PooledWebSocketConnection>,
        lease: WebSocketPoolLease,
    },
    Connect(WebSocketPoolConnectLease),
    Wait(WebSocketPoolConnectWaiter),
    Bypass(WebSocketPoolBypassReason),
}

struct WebSocketPoolConnecting {
    id: Uuid,
    started_at: Instant,
    outcome: watch::Sender<WebSocketPoolConnectOutcome>,
    cancellation: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebSocketPoolConnectOutcome {
    Pending,
    Ready,
    Failed,
}

pub(crate) struct WebSocketPoolConnectWaiter {
    receiver: watch::Receiver<WebSocketPoolConnectOutcome>,
    started_at: Instant,
}

impl WebSocketPoolConnectWaiter {
    pub(crate) fn remaining_budget(&self, budget: Duration) -> Duration {
        budget.saturating_sub(self.started_at.elapsed())
    }

    pub(crate) async fn wait(mut self) -> WebSocketPoolConnectOutcome {
        loop {
            let outcome = *self.receiver.borrow_and_update();
            if outcome != WebSocketPoolConnectOutcome::Pending {
                return outcome;
            }
            if self.receiver.changed().await.is_err() {
                return WebSocketPoolConnectOutcome::Failed;
            }
        }
    }
}

pub(crate) struct WebSocketPoolConnectLease {
    pool: CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    id: Uuid,
    started_at: Instant,
    outcome: watch::Sender<WebSocketPoolConnectOutcome>,
    cancellation: CancellationToken,
    // slot 分配时即注册，封闭 acquire 与后台 task spawn 之间的 shutdown 竞态。
    _task_registration: TaskTrackerToken,
    armed: bool,
}

impl WebSocketPoolConnectLease {
    fn reserve(pool: CodexWebSocketPool, key: CodexWebSocketPoolKey) -> Self {
        let (outcome, _) = watch::channel(WebSocketPoolConnectOutcome::Pending);
        let cancellation = pool.shutdown.child_token();
        let task_registration = pool.tasks.token();
        Self {
            pool,
            key,
            id: Uuid::new_v4(),
            started_at: Instant::now(),
            outcome,
            cancellation,
            _task_registration: task_registration,
            armed: true,
        }
    }

    pub(crate) fn started_at(&self) -> Instant {
        self.started_at
    }

    pub(crate) fn key(&self) -> &CodexWebSocketPoolKey {
        &self.key
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) async fn connected_idle(
        mut self,
        connection: PooledWebSocketConnection,
    ) -> Result<(), Box<PooledWebSocketConnection>> {
        let result = self
            .pool
            .finish_connect_idle(&self.key, self.id, connection)
            .await;
        let outcome = if result.is_ok() {
            WebSocketPoolConnectOutcome::Ready
        } else {
            WebSocketPoolConnectOutcome::Failed
        };
        self.outcome.send_replace(outcome);
        self.armed = false;
        result
    }

    pub(crate) async fn failed(mut self) {
        self.cancellation.cancel();
        self.pool.fail_connect(&self.key, self.id).await;
        self.outcome
            .send_replace(WebSocketPoolConnectOutcome::Failed);
        self.armed = false;
    }
}

impl Drop for WebSocketPoolConnectLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancellation.cancel();
        self.outcome
            .send_replace(WebSocketPoolConnectOutcome::Failed);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let pool = self.pool.clone();
        let key = self.key.clone();
        let connect_id = self.id;
        runtime.spawn(async move {
            pool.fail_connect(&key, connect_id).await;
        });
    }
}

#[derive(Clone, Copy)]
struct WebSocketPoolReservation {
    id: Uuid,
    reserved_at: Instant,
}

pub(crate) struct WebSocketPoolLease {
    pool: CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    reservation: WebSocketPoolReservation,
    armed: bool,
}

impl WebSocketPoolLease {
    fn reserve(pool: CodexWebSocketPool, key: CodexWebSocketPoolKey) -> Self {
        Self {
            pool,
            key,
            reservation: WebSocketPoolReservation {
                id: Uuid::new_v4(),
                reserved_at: Instant::now(),
            },
            armed: true,
        }
    }

    pub(crate) async fn put(mut self, connection: PooledWebSocketConnection) {
        self.pool
            .put_reserved(&self.key, self.reservation.id, connection)
            .await;
        self.armed = false;
    }

    pub(crate) async fn discard(mut self) {
        self.pool
            .discard_reserved(&self.key, self.reservation.id)
            .await;
        self.armed = false;
    }
}

impl Drop for WebSocketPoolLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let pool = self.pool.clone();
        let key = self.key.clone();
        let reservation_id = self.reservation.id;
        runtime.spawn(async move {
            pool.discard_reserved(&key, reservation_id).await;
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketPoolBypassReason {
    Disabled,
    Busy,
    Cap,
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

fn short_sha256<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize()).chars().take(12).collect()
}

fn account_slot_count(
    slots: &HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    account_id: &str,
) -> usize {
    slots
        .keys()
        .filter(|key| key.account_id() == account_id)
        .count()
}

async fn close_pooled_connection(connection: PooledWebSocketConnection) {
    connection.websocket.close().await;
}

async fn close_pooled_connections(connections: Vec<PooledWebSocketConnection>) {
    for connection in connections {
        close_pooled_connection(connection).await;
    }
}

/// idle 连接是否应从池中摘除：被后台 pump 标记死亡，或已超过 `max_age`。
fn should_close_idle_connection(
    connection: &PooledWebSocketConnection,
    now: Instant,
    max_age: Duration,
) -> bool {
    connection.websocket.is_closed() || now.duration_since(connection.created_at) >= max_age
}
