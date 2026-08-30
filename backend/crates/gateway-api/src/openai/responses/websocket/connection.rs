//! 下游客户端 WebSocket 的单 owner pump 与有界收发边界。

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{Sink, SinkExt, Stream, StreamExt};
use gateway_core::engine::CancellationToken;
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};

const OUTBOUND_COMMAND_BUFFER: usize = 32;
const INBOUND_EVENT_BUFFER: usize = 32;
const DOWNSTREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECTION_MAX_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy)]
pub(super) struct ConnectionConfig {
    pub(super) write_timeout: Duration,
    pub(super) max_age: Duration,
}

impl ConnectionConfig {
    pub(super) const PRODUCTION: Self = Self {
        write_timeout: DOWNSTREAM_WRITE_TIMEOUT,
        max_age: CONNECTION_MAX_AGE,
    };
}

/// 业务层可观察的客户端输入；Ping/Pong 始终由 pump 消费。
pub(super) enum ConnectionEvent {
    Text(String),
    Binary,
    Expired,
    Exited(PumpExitReason),
}

/// 下游写入阶段；名称刻意使用 write，而不是暗示客户端已消费的 delivery。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FramePhase {
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

#[derive(Clone)]
pub(super) struct WriteContext {
    request_id: Option<Arc<str>>,
    phase: FramePhase,
}

impl WriteContext {
    pub(super) fn request(request_id: &Arc<str>, phase: FramePhase) -> Self {
        Self {
            request_id: Some(Arc::clone(request_id)),
            phase,
        }
    }

    pub(super) const fn connection(phase: FramePhase) -> Self {
        Self {
            request_id: None,
            phase,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PumpExitReason {
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
        }
    }
}

#[derive(Debug, Error)]
pub(super) enum ConnectionWriteError {
    #[error("downstream WebSocket pump is closed")]
    Closed,
    #[error("downstream WebSocket write timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("downstream WebSocket transport write failed: {message}")]
    Transport { message: String },
}

struct ConnectionCommand {
    message: Message,
    context: WriteContext,
    acknowledged: oneshot::Sender<Result<(), ConnectionWriteError>>,
}

#[derive(Default)]
struct ConnectionStats {
    command_queue_high_water: AtomicUsize,
    command_backpressure_count: AtomicU64,
    ping_received_count: AtomicU64,
    pong_written_count: AtomicU64,
}

impl ConnectionStats {
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

pub(super) struct ResponsesWebSocketConnection {
    connection_id: Arc<str>,
    opened_at: Instant,
    expired: Arc<AtomicBool>,
    commands: Option<mpsc::Sender<ConnectionCommand>>,
    incoming: mpsc::Receiver<ConnectionEvent>,
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
    ) -> Self {
        spawn_connection(
            socket,
            Arc::<str>::from(connection_id),
            cancellation,
            ConnectionConfig::PRODUCTION,
        )
    }

    pub(super) fn id(&self) -> &str {
        &self.connection_id
    }

    pub(super) fn is_expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }

    pub(super) fn age(&self) -> Duration {
        self.opened_at.elapsed()
    }

    pub(super) async fn next_event(&mut self) -> Option<ConnectionEvent> {
        let event = self.incoming.recv().await;
        if let Some(ConnectionEvent::Exited(reason)) = event.as_ref() {
            self.exit_reason.get_or_insert(*reason);
        } else if event.is_none() {
            self.exit_reason.get_or_insert(PumpExitReason::PumpStopped);
        }
        event
    }

    pub(super) async fn send_text(
        &mut self,
        payload: String,
        context: WriteContext,
    ) -> Result<(), ConnectionWriteError> {
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
    ) -> Result<(), ConnectionWriteError> {
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
            Ok(()) if phase.is_milestone() => tracing::info!(
                websocket_connection_id = %self.connection_id,
                request_id = request_id.as_deref().unwrap_or(""),
                frame_phase = phase.as_str(),
                write_duration_ms = duration.as_millis(),
                "Responses WebSocket frame write succeeded"
            ),
            Ok(()) => tracing::debug!(
                websocket_connection_id = %self.connection_id,
                request_id = request_id.as_deref().unwrap_or(""),
                frame_phase = phase.as_str(),
                write_duration_ms = duration.as_millis(),
                "Responses WebSocket frame write succeeded"
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
        tracing::info!(
            websocket_connection_id = %self.connection_id,
            request_count,
            connection_age_ms = self.age().as_millis(),
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
            pong_written_count = self.stats.pong_written_count.load(Ordering::Relaxed),
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

pub(super) fn spawn_connection<S, E>(
    socket: S,
    connection_id: Arc<str>,
    cancellation: CancellationToken,
    config: ConnectionConfig,
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
    let pump_task = tokio::spawn(run_pump(
        socket,
        command_rx,
        incoming_tx,
        Arc::clone(&connection_id),
        cancellation,
        opened_at,
        Arc::clone(&expired),
        Arc::clone(&stats),
        config,
    ));
    ResponsesWebSocketConnection {
        connection_id,
        opened_at,
        expired,
        commands: Some(command_tx),
        incoming: incoming_rx,
        pump_task: Some(pump_task),
        stats,
        config,
        exit_reason: None,
    }
}

#[expect(clippy::too_many_arguments)]
async fn run_pump<S, E>(
    mut socket: S,
    mut commands: mpsc::Receiver<ConnectionCommand>,
    incoming: mpsc::Sender<ConnectionEvent>,
    connection_id: Arc<str>,
    cancellation: CancellationToken,
    opened_at: Instant,
    expired: Arc<AtomicBool>,
    stats: Arc<ConnectionStats>,
    config: ConnectionConfig,
) where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin,
    E: fmt::Display,
{
    let deadline = tokio::time::sleep_until(opened_at + config.max_age);
    tokio::pin!(deadline);
    let mut deadline_elapsed = false;
    let reason = loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => break PumpExitReason::LifecycleShutdown,
            () = &mut deadline, if !deadline_elapsed => {
                deadline_elapsed = true;
                expired.store(true, Ordering::Release);
                if let Err(reason) = emit_incoming(&incoming, ConnectionEvent::Expired) {
                    break reason;
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break PumpExitReason::CoordinatorDropped;
                };
                let ConnectionCommand {
                    message,
                    context,
                    acknowledged,
                } = command;
                let closing = matches!(message, Message::Close(_));
                let result = socket.send(message).await.map_err(|error| {
                    let message = error.to_string();
                    tracing::debug!(
                        websocket_connection_id = %connection_id,
                        request_id = context.request_id.as_deref().unwrap_or(""),
                        frame_phase = context.phase.as_str(),
                        error = %message,
                        "Responses WebSocket pump transport write failed"
                    );
                    ConnectionWriteError::Transport { message }
                });
                let failed = result.is_err();
                let _ = acknowledged.send(result);
                if failed {
                    break PumpExitReason::WriteError;
                }
                if closing {
                    break PumpExitReason::ServerClose;
                }
            }
            message = socket.next() => match message {
                Some(Ok(Message::Text(payload))) => {
                    if let Err(reason) = emit_incoming(&incoming, ConnectionEvent::Text(payload.to_string())) {
                        break reason;
                    }
                }
                Some(Ok(Message::Binary(_))) => {
                    if let Err(reason) = emit_incoming(&incoming, ConnectionEvent::Binary) {
                        break reason;
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    stats.ping_received_count.fetch_add(1, Ordering::Relaxed);
                    match timeout(config.write_timeout, socket.send(Message::Pong(payload))).await {
                        Ok(Ok(())) => {
                            stats.pong_written_count.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(Err(error)) => {
                            tracing::info!(
                                websocket_connection_id = %connection_id,
                                error = %error,
                                "Responses WebSocket Pong write failed"
                            );
                            break PumpExitReason::WriteError;
                        }
                        Err(_) => break PumpExitReason::WriteTimeout,
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) => break PumpExitReason::ClientClose,
                Some(Err(error)) => {
                    tracing::info!(
                        websocket_connection_id = %connection_id,
                        error = %error,
                        "Responses WebSocket receive failed"
                    );
                    break PumpExitReason::ReadError;
                }
                None => break PumpExitReason::PeerEof,
            },
        }
    };
    let _ = incoming.try_send(ConnectionEvent::Exited(reason));
    tracing::debug!(
        websocket_connection_id = %connection_id,
        pump_exit_reason = reason.as_str(),
        "Responses WebSocket pump exited"
    );
}

fn emit_incoming(
    incoming: &mpsc::Sender<ConnectionEvent>,
    event: ConnectionEvent,
) -> Result<(), PumpExitReason> {
    incoming.try_send(event).map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => PumpExitReason::InboundOverload,
        mpsc::error::TrySendError::Closed(_) => PumpExitReason::CoordinatorDropped,
    })
}
