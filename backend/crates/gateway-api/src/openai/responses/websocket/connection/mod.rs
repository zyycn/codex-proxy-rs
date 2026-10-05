//! 下游客户端 WebSocket 的单 owner pump 与有界收发边界

mod middleware;

use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{Sink, SinkExt, Stream, StreamExt};
use gateway_core::{
    engine::middleware::{FrozenMiddlewarePlan, MiddlewareHeader},
    lifecycle::CancellationToken,
    middleware::websocket as contract,
};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};

const OUTBOUND_COMMAND_BUFFER: usize = 32;
const INBOUND_EVENT_BUFFER: usize = 32;
const DOWNSTREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECTION_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const DOWNSTREAM_PING_INTERVAL: Duration = Duration::from_secs(25);

/// WebSocket pump 的写入与生命周期预算
#[derive(Clone, Copy)]
pub struct ConnectionConfig {
    write_timeout: Duration,
    max_age: Duration,
}

impl ConnectionConfig {
    /// 生产环境固定预算
    pub const PRODUCTION: Self = Self {
        write_timeout: DOWNSTREAM_WRITE_TIMEOUT,
        max_age: CONNECTION_MAX_AGE,
    };
}

/// 业务层可观察的客户端输入；Ping/Pong 始终由 pump 消费
pub enum ConnectionEvent {
    Text(String),
    Binary,
    Expired,
    Exited(PumpExitReason),
}

/// 接收队列与活动期间暂存的请求共用容量；移动帧不释放它占用的名额
pub(super) struct PendingConnectionEvent {
    pub(super) event: ConnectionEvent,
    _permit: Option<OwnedSemaphorePermit>,
}

impl PendingConnectionEvent {
    fn exited(reason: PumpExitReason) -> Self {
        Self {
            event: ConnectionEvent::Exited(reason),
            _permit: None,
        }
    }
}

/// 下游写入阶段；名称刻意使用 write，而不是暗示客户端已消费的 delivery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePhase {
    Metadata,
    First,
    Data,
    Terminal,
    FirstAndTerminal,
    Error,
    ConnectionLimit,
    Close,
}

impl FramePhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::First => "first",
            Self::Data => "data",
            Self::Terminal => "terminal",
            Self::FirstAndTerminal => "first_and_terminal",
            Self::Error => "error",
            Self::ConnectionLimit => "connection_limit",
            Self::Close => "close",
        }
    }

    const fn is_milestone(self) -> bool {
        matches!(
            self,
            Self::First
                | Self::Terminal
                | Self::FirstAndTerminal
                | Self::Error
                | Self::ConnectionLimit
                | Self::Close
        )
    }
}

/// 一次下游写入的请求归属与协议阶段
#[derive(Clone)]
pub struct WriteContext {
    request_id: Option<Arc<str>>,
    phase: FramePhase,
}

impl WriteContext {
    /// 创建归属于某个请求的写入上下文
    #[must_use]
    pub fn request(request_id: &Arc<str>, phase: FramePhase) -> Self {
        Self {
            request_id: Some(Arc::clone(request_id)),
            phase,
        }
    }

    /// 创建连接级写入上下文
    #[must_use]
    pub const fn connection(phase: FramePhase) -> Self {
        Self {
            request_id: None,
            phase,
        }
    }
}

/// WebSocket pump 停止的稳定原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpExitReason {
    ClientClose,
    PeerEof,
    ReadError,
    WriteError,
    WriteTimeout,
    LifecycleShutdown,
    ConnectionMaxAge,
    CoordinatorDropped,
    InboundOverload,
    ServerClose,
    PumpStopped,
    MiddlewareFailed,
}

impl PumpExitReason {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ClientClose => "client_close",
            Self::PeerEof => "peer_eof",
            Self::ReadError => "read_error",
            Self::WriteError => "write_error",
            Self::WriteTimeout => "write_timeout",
            Self::LifecycleShutdown => "lifecycle_shutdown",
            Self::ConnectionMaxAge => "connection_max_age",
            Self::CoordinatorDropped => "coordinator_dropped",
            Self::InboundOverload => "inbound_overload",
            Self::ServerClose => "server_close",
            Self::PumpStopped => "pump_stopped",
            Self::MiddlewareFailed => "middleware_failed",
        }
    }
}

/// 下游 WebSocket 写入失败
#[derive(Debug, Error)]
pub enum ConnectionWriteError {
    #[error("downstream WebSocket pump is closed")]
    Closed,
    #[error("downstream WebSocket write timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("downstream WebSocket transport write failed: {message}")]
    Transport { message: String },
}

/// 只有实际 transport 写入才记为 Written；插件丢弃消息不伪造写入成功
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    Suppressed,
}

struct ConnectionCommand {
    message: Message,
    context: WriteContext,
    acknowledged: oneshot::Sender<Result<WriteOutcome, ConnectionWriteError>>,
}

#[derive(Default)]
struct ConnectionStats {
    command_queue_high_water: AtomicUsize,
    command_backpressure_count: AtomicU64,
    ping_received_count: AtomicU64,
    ping_written_count: AtomicU64,
    pong_received_count: AtomicU64,
    last_read_ms: AtomicU64,
    last_write_ms: AtomicU64,
}

impl ConnectionStats {
    fn record_read(&self, opened_at: Instant) {
        self.last_read_ms.store(
            u64::try_from(opened_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn record_write(&self, opened_at: Instant) {
        self.last_write_ms.store(
            u64::try_from(opened_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn observe_command_queue(&self, sender: &mpsc::Sender<ConnectionCommand>) {
        let queued = OUTBOUND_COMMAND_BUFFER
            .saturating_sub(sender.capacity())
            .saturating_add(1)
            .min(OUTBOUND_COMMAND_BUFFER);
        self.command_queue_high_water
            .fetch_max(queued, Ordering::Relaxed);
        if sender.capacity() == 0 {
            self.command_backpressure_count
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 协调层持有的单 owner WebSocket 连接句柄
pub struct ResponsesWebSocketConnection {
    connection_id: Arc<str>,
    opened_at: Instant,
    expired: Arc<AtomicBool>,
    commands: Option<mpsc::Sender<ConnectionCommand>>,
    incoming: mpsc::Receiver<PendingConnectionEvent>,
    deferred: VecDeque<PendingConnectionEvent>,
    exited: oneshot::Receiver<PumpExitReason>,
    pump_task: Option<JoinHandle<()>>,
    stats: Arc<ConnectionStats>,
    config: ConnectionConfig,
    exit_reason: Option<PumpExitReason>,
}

impl ResponsesWebSocketConnection {
    pub(super) fn new(
        socket: WebSocket,
        connection_id: String,
        cancellation: CancellationToken,
        middleware: Option<FrozenMiddlewarePlan>,
        headers: Arc<[MiddlewareHeader]>,
    ) -> Self {
        spawn_with_middleware(
            socket,
            Arc::<str>::from(connection_id),
            cancellation,
            ConnectionConfig::PRODUCTION,
            middleware,
            headers,
        )
    }

    pub(super) fn id(&self) -> &str {
        &self.connection_id
    }

    /// 返回连接是否已经达到生命周期上限
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }

    pub(super) fn age(&self) -> Duration {
        self.opened_at.elapsed()
    }

    /// 等待下一个需要业务层处理的客户端事件
    pub async fn next_event(&mut self) -> Option<ConnectionEvent> {
        if self.exit_reason.is_none() {
            match self.exited.try_recv() {
                Ok(reason) => {
                    self.exit_reason = Some(reason);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.exit_reason = Some(PumpExitReason::PumpStopped);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if let Some(reason) = self.exit_reason {
            return Some(ConnectionEvent::Exited(reason));
        }
        if let Some(event) = self.deferred.pop_front() {
            return Some(event.event);
        }
        self.next_active_event().await.map(|event| event.event)
    }

    /// 活动响应期间只消费新到的帧，已排队的下一轮请求继续保持串行
    pub(super) async fn next_active_event(&mut self) -> Option<PendingConnectionEvent> {
        if let Some(reason) = self.exit_reason {
            return Some(PendingConnectionEvent::exited(reason));
        }
        let event = tokio::select! {
            biased;
            reason = &mut self.exited => {
                Some(PendingConnectionEvent::exited(reason.unwrap_or(PumpExitReason::PumpStopped)))
            }
            event = self.incoming.recv() => event,
        };
        if let Some(PendingConnectionEvent {
            event: ConnectionEvent::Exited(reason),
            ..
        }) = event.as_ref()
        {
            self.exit_reason.get_or_insert(*reason);
        } else if event.is_none() {
            self.exit_reason.get_or_insert(PumpExitReason::PumpStopped);
        }
        event
    }

    pub(super) fn defer(&mut self, event: PendingConnectionEvent) {
        self.deferred.push_back(event);
    }

    /// 等待连接退出，不消费留给后续串行请求的业务帧
    pub async fn wait_for_exit(&mut self) -> PumpExitReason {
        if let Some(reason) = self.exit_reason {
            return reason;
        }
        let reason = (&mut self.exited)
            .await
            .unwrap_or(PumpExitReason::PumpStopped);
        self.exit_reason = Some(reason);
        reason
    }

    /// 串行写入文本帧，并等待 pump 确认 transport 写入结果
    ///
    /// # Errors
    ///
    /// pump 已关闭、写入超时或 transport 失败时返回稳定错误
    pub async fn send_text(
        &mut self,
        payload: String,
        context: WriteContext,
    ) -> Result<WriteOutcome, ConnectionWriteError> {
        self.send(Message::Text(payload.into()), context).await
    }

    pub(super) async fn close_policy(
        &mut self,
        reason: &'static str,
        request_id: Option<&Arc<str>>,
    ) {
        let context = request_id.map_or_else(
            || WriteContext::connection(FramePhase::Close),
            |request_id| WriteContext::request(request_id, FramePhase::Close),
        );
        self.close(
            CloseFrame {
                code: close_code::POLICY,
                reason: reason.into(),
            },
            context,
            PumpExitReason::ServerClose,
        )
        .await;
    }

    pub(super) async fn close_for_connection_limit(&mut self, reason: &'static str) {
        self.close(
            CloseFrame {
                code: close_code::NORMAL,
                reason: reason.into(),
            },
            WriteContext::connection(FramePhase::Close),
            PumpExitReason::ConnectionMaxAge,
        )
        .await;
    }

    async fn close(
        &mut self,
        frame: CloseFrame,
        context: WriteContext,
        exit_reason: PumpExitReason,
    ) {
        if self
            .send(Message::Close(Some(frame)), context)
            .await
            .is_ok()
        {
            self.exit_reason.get_or_insert(exit_reason);
        }
    }

    async fn send(
        &mut self,
        message: Message,
        context: WriteContext,
    ) -> Result<WriteOutcome, ConnectionWriteError> {
        let Some(commands) = self.commands.as_ref().cloned() else {
            return Err(ConnectionWriteError::Closed);
        };
        self.stats.observe_command_queue(&commands);
        let (acknowledged, acknowledgement) = oneshot::channel();
        let started_at = Instant::now();
        let request_id = context.request_id.clone();
        let phase = context.phase;
        let command = ConnectionCommand {
            message,
            context,
            acknowledged,
        };
        let write = async move {
            commands
                .send(command)
                .await
                .map_err(|_| ConnectionWriteError::Closed)?;
            acknowledgement
                .await
                .map_err(|_| ConnectionWriteError::Closed)?
        };
        let result = match timeout(self.config.write_timeout, write).await {
            Ok(result) => result,
            Err(_) => Err(ConnectionWriteError::Timeout {
                timeout: self.config.write_timeout,
            }),
        };
        let duration = started_at.elapsed();
        match &result {
            Ok(WriteOutcome::Written) if phase.is_milestone() => tracing::info!(
                websocket_connection_id = %self.connection_id,
                request_id = request_id.as_deref().unwrap_or(""),
                frame_phase = phase.as_str(),
                write_duration_ms = duration.as_millis(),
                "Responses WebSocket frame write succeeded"
            ),
            Ok(outcome) => tracing::debug!(
                websocket_connection_id = %self.connection_id,
                request_id = request_id.as_deref().unwrap_or(""),
                frame_phase = phase.as_str(),
                write_duration_ms = duration.as_millis(),
                ?outcome,
                "Responses WebSocket frame processed"
            ),
            Err(error) => tracing::info!(
                websocket_connection_id = %self.connection_id,
                request_id = request_id.as_deref().unwrap_or(""),
                frame_phase = phase.as_str(),
                write_duration_ms = duration.as_millis(),
                error = %error,
                "Responses WebSocket frame write failed"
            ),
        }
        if let Err(error) = &result {
            let reason = match error {
                ConnectionWriteError::Timeout { .. } => PumpExitReason::WriteTimeout,
                ConnectionWriteError::Transport { .. } => PumpExitReason::WriteError,
                ConnectionWriteError::Closed => PumpExitReason::PumpStopped,
            };
            self.terminate(reason);
        }
        result
    }

    fn terminate(&mut self, reason: PumpExitReason) {
        self.exit_reason.get_or_insert(reason);
        self.commands.take();
        if let Some(task) = self.pump_task.take() {
            task.abort();
        }
    }

    pub(super) fn log_summary(&self, request_count: u64) {
        let connection_age_ms = self.age().as_millis();
        let read_idle_ms = connection_age_ms
            .saturating_sub(u128::from(self.stats.last_read_ms.load(Ordering::Relaxed)));
        let write_idle_ms = connection_age_ms
            .saturating_sub(u128::from(self.stats.last_write_ms.load(Ordering::Relaxed)));
        tracing::info!(
            websocket_connection_id = %self.connection_id,
            request_count,
            connection_age_ms,
            pump_exit_reason = self
                .exit_reason
                .map_or("unknown", PumpExitReason::as_str),
            command_queue_high_water = self
                .stats
                .command_queue_high_water
                .load(Ordering::Relaxed),
            command_backpressure_count = self
                .stats
                .command_backpressure_count
                .load(Ordering::Relaxed),
            ping_received_count = self.stats.ping_received_count.load(Ordering::Relaxed),
            ping_written_count = self.stats.ping_written_count.load(Ordering::Relaxed),
            pong_received_count = self.stats.pong_received_count.load(Ordering::Relaxed),
            read_idle_ms,
            write_idle_ms,
            "Responses WebSocket disconnected"
        );
    }
}

impl Drop for ResponsesWebSocketConnection {
    fn drop(&mut self) {
        self.commands.take();
        if let Some(task) = self.pump_task.take() {
            task.abort();
        }
    }
}

/// 为自动处理 Ping/Pong 的 WebSocket transport 启动单 owner pump
pub fn spawn_connection<S, E>(
    socket: S,
    connection_id: Arc<str>,
    cancellation: CancellationToken,
    config: ConnectionConfig,
) -> ResponsesWebSocketConnection
where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    spawn_with_middleware(
        socket,
        connection_id,
        cancellation,
        config,
        None,
        Arc::from([]),
    )
}

fn spawn_with_middleware<S, E>(
    socket: S,
    connection_id: Arc<str>,
    cancellation: CancellationToken,
    config: ConnectionConfig,
    middleware: Option<FrozenMiddlewarePlan>,
    headers: Arc<[MiddlewareHeader]>,
) -> ResponsesWebSocketConnection
where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    let opened_at = Instant::now();
    let expired = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(ConnectionStats::default());
    let (command_tx, command_rx) = mpsc::channel(OUTBOUND_COMMAND_BUFFER);
    let (incoming_tx, incoming_rx) = mpsc::channel(INBOUND_EVENT_BUFFER);
    let (exit_tx, exit_rx) = oneshot::channel();
    let pump_task = tokio::spawn(run_pump(
        socket,
        command_rx,
        incoming_tx,
        exit_tx,
        Arc::clone(&connection_id),
        cancellation,
        opened_at,
        Arc::clone(&expired),
        Arc::clone(&stats),
        config,
        middleware,
        headers,
    ));
    ResponsesWebSocketConnection {
        connection_id,
        opened_at,
        expired,
        commands: Some(command_tx),
        incoming: incoming_rx,
        deferred: VecDeque::new(),
        exited: exit_rx,
        pump_task: Some(pump_task),
        stats,
        config,
        exit_reason: None,
    }
}

#[expect(clippy::too_many_arguments)]
async fn run_pump<S, E>(
    socket: S,
    commands: mpsc::Receiver<ConnectionCommand>,
    incoming: mpsc::Sender<PendingConnectionEvent>,
    exited: oneshot::Sender<PumpExitReason>,
    connection_id: Arc<str>,
    cancellation: CancellationToken,
    opened_at: Instant,
    expired: Arc<AtomicBool>,
    stats: Arc<ConnectionStats>,
    config: ConnectionConfig,
    middleware: Option<FrozenMiddlewarePlan>,
    headers: Arc<[MiddlewareHeader]>,
) where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    let context = Arc::new(PumpContext {
        connection_id,
        cancellation: cancellation.child_token(),
        opened_at,
        expired,
        stats,
        config,
        middleware,
        headers,
    });
    let _lifetime = middleware::CancelOnDrop(Some(context.cancellation.clone()));
    let (writer, mut reader) = socket.split();
    let writer = Arc::new(middleware::Writer::new(writer, context.clone()));
    let sender: Arc<dyn contract::Sender> = writer.clone();
    // 同一个受管任务同时驱动两侧；等待写入不能阻塞入站控制、关闭或取消
    let reason = tokio::select! {
        biased;
        reason = read_connection(&mut reader, &incoming, sender.clone(), &context) => reason,
        reason = write_connection(writer.clone(), commands, &context) => reason,
    };
    context.cancellation.cancel();
    let _ = exited.send(reason);
    tracing::debug!(websocket_connection_id = %context.connection_id, pump_exit_reason = reason.as_str(), "Responses WebSocket pump exited");
}

struct PumpContext {
    middleware: Option<FrozenMiddlewarePlan>,
    headers: Arc<[MiddlewareHeader]>,
    connection_id: Arc<str>,
    cancellation: CancellationToken,
    opened_at: Instant,
    expired: Arc<AtomicBool>,
    stats: Arc<ConnectionStats>,
    config: ConnectionConfig,
}

async fn read_connection<S, E>(
    socket: &mut S,
    incoming: &mpsc::Sender<PendingConnectionEvent>,
    sender: Arc<dyn contract::Sender>,
    context: &PumpContext,
) -> PumpExitReason
where
    S: Stream<Item = Result<Message, E>> + Unpin,
    E: fmt::Display,
{
    let event_slots = Arc::new(Semaphore::new(INBOUND_EVENT_BUFFER));
    let deadline = tokio::time::sleep_until(context.opened_at + context.config.max_age);
    tokio::pin!(deadline);
    let mut deadline_elapsed = false;
    loop {
        tokio::select! {
            biased;
            () = context.cancellation.cancelled() => return PumpExitReason::LifecycleShutdown,
            () = &mut deadline, if !deadline_elapsed => {
                deadline_elapsed = true;
                context.expired.store(true, Ordering::Release);
                // 到期只阻止下一轮，当前响应仍由协调层按原有合同收尾
                match emit_incoming(incoming, &event_slots, ConnectionEvent::Expired) {
                    Ok(()) | Err(PumpExitReason::InboundOverload) => {}
                    Err(reason) => return reason,
                }
            }
            message = socket.next() => {
                if matches!(&message, Some(Ok(_))) { context.stats.record_read(context.opened_at); }
                let message = match message {
                    Some(Ok(message)) => match middleware::transform(message, contract::Direction::Incoming, sender.clone(), context).await {
                        Ok(Some(message)) => Some(Ok(message)),
                        Ok(None) => continue,
                        Err(_) => return PumpExitReason::MiddlewareFailed,
                    },
                    other => other,
                };
                let event = match message {
                    Some(Ok(Message::Text(payload))) => Some(ConnectionEvent::Text(payload.to_string())),
                    Some(Ok(Message::Binary(_))) => Some(ConnectionEvent::Binary),
                    Some(Ok(Message::Ping(_))) => {
                        // Axum/tungstenite 在继续读取时自动刷新 Pong，不能再手工发送一份
                        context.stats.ping_received_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    Some(Ok(Message::Pong(_))) => {
                        context.stats.pong_received_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    Some(Ok(Message::Close(_))) => return PumpExitReason::ClientClose,
                    Some(Err(error)) => {
                        tracing::info!(websocket_connection_id = %context.connection_id, error = %error, "Responses WebSocket receive failed");
                        return PumpExitReason::ReadError;
                    }
                    None => return PumpExitReason::PeerEof,
                };
                if let Some(event) = event && let Err(reason) = emit_incoming(incoming, &event_slots, event) { return reason; }
            }
        }
    }
}

async fn write_connection<S, E>(
    writer: Arc<middleware::Writer<S>>,
    mut commands: mpsc::Receiver<ConnectionCommand>,
    context: &PumpContext,
) -> PumpExitReason
where
    S: Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    let mut heartbeat = tokio::time::interval_at(
        context.opened_at + DOWNSTREAM_PING_INTERVAL,
        DOWNSTREAM_PING_INTERVAL,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ping_sequence = 0_u64;
    loop {
        let message = tokio::select! {
            biased;
            () = context.cancellation.cancelled() => return PumpExitReason::LifecycleShutdown,
            command = commands.recv() => {
                let Some(command) = command else { return PumpExitReason::CoordinatorDropped; };
                let ConnectionCommand { message, context: write_context, acknowledged } = command;
                let closing = matches!(message, Message::Close(_));
                let result = match middleware::transform(message, contract::Direction::Outgoing, writer.clone(), context).await {
                    Ok(Some(message)) => writer.write(message).await.map(|()| WriteOutcome::Written),
                    Ok(None) => Ok(WriteOutcome::Suppressed),
                    Err(error) => Err(ConnectionWriteError::Transport { message: error.to_string() }),
                };
                let reason = result.as_ref().err().map(|error| write_exit_reason(error, context));
                if let Err(error) = &result {
                    tracing::debug!(websocket_connection_id = %context.connection_id, request_id = write_context.request_id.as_deref().unwrap_or(""), frame_phase = write_context.phase.as_str(), error = %error, "Responses WebSocket pump transport write failed");
                }
                let _ = acknowledged.send(result);
                if let Some(reason) = reason { return reason; }
                if closing { return PumpExitReason::ServerClose; }
                continue;
            }
            _ = heartbeat.tick() => {
                ping_sequence = ping_sequence.wrapping_add(1);
                Message::Ping(ping_sequence.to_be_bytes().to_vec().into())
            }
        };
        let message = match middleware::transform(
            message,
            contract::Direction::Outgoing,
            writer.clone(),
            context,
        )
        .await
        {
            Ok(Some(message)) => message,
            Ok(None) => continue,
            Err(_) => return PumpExitReason::MiddlewareFailed,
        };
        if let Err(error) = writer.write(message).await {
            tracing::info!(websocket_connection_id = %context.connection_id, error = %error, "Responses WebSocket control write failed");
            return write_exit_reason(&error, context);
        }
        context
            .stats
            .ping_written_count
            .fetch_add(1, Ordering::Relaxed);
    }
}

async fn write_message<S, E>(
    socket: &mut S,
    message: Message,
    context: &PumpContext,
) -> Result<(), ConnectionWriteError>
where
    S: Sink<Message, Error = E> + Unpin,
    E: fmt::Display,
{
    tokio::select! {
        biased;
        () = context.cancellation.cancelled() => Err(ConnectionWriteError::Closed),
        result = timeout(context.config.write_timeout, socket.send(message)) => {
            match result {
                Ok(Ok(())) => { context.stats.record_write(context.opened_at); Ok(()) }
                Ok(Err(error)) => Err(ConnectionWriteError::Transport { message: error.to_string() }),
                Err(_) => Err(ConnectionWriteError::Timeout { timeout: context.config.write_timeout }),
            }
        }
    }
}

fn write_exit_reason(error: &ConnectionWriteError, context: &PumpContext) -> PumpExitReason {
    match error {
        ConnectionWriteError::Timeout { .. } => PumpExitReason::WriteTimeout,
        ConnectionWriteError::Transport { .. } => PumpExitReason::WriteError,
        ConnectionWriteError::Closed if context.cancellation.is_cancelled() => {
            PumpExitReason::LifecycleShutdown
        }
        ConnectionWriteError::Closed => PumpExitReason::PumpStopped,
    }
}

fn emit_incoming(
    incoming: &mpsc::Sender<PendingConnectionEvent>,
    slots: &Arc<Semaphore>,
    event: ConnectionEvent,
) -> Result<(), PumpExitReason> {
    let permit = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| PumpExitReason::InboundOverload)?;
    incoming
        .try_send(PendingConnectionEvent {
            event,
            _permit: Some(permit),
        })
        .map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => PumpExitReason::InboundOverload,
            mpsc::error::TrySendError::Closed(_) => PumpExitReason::CoordinatorDropped,
        })
}
