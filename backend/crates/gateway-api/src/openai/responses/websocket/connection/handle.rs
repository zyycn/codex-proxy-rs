//! 协调层持有的连接句柄与唯一 pump 启动入口

use super::{
    frame::*,
    pump::run_pump,
    state::{ConnectionCommand, ConnectionStats},
};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures::{Sink, Stream};
use gateway_core::{
    engine::middleware::{FrozenMiddlewarePlan, MiddlewareHeader},
    lifecycle::CancellationToken,
};
use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};

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
    pub(in super::super) fn new(
        socket: WebSocket,
        connection_id: String,
        cancellation: CancellationToken,
        middleware: Option<FrozenMiddlewarePlan>,
        headers: Arc<[MiddlewareHeader]>,
    ) -> Self {
        spawn_connection(
            socket,
            Arc::<str>::from(connection_id),
            cancellation,
            ConnectionConfig::PRODUCTION,
            middleware,
            headers,
        )
    }

    pub(in super::super) fn id(&self) -> &str {
        &self.connection_id
    }

    /// 返回连接是否已经达到生命周期上限
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }

    pub(in super::super) fn age(&self) -> Duration {
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
    pub(in super::super) async fn next_active_event(&mut self) -> Option<PendingConnectionEvent> {
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

    pub(in super::super) fn defer(&mut self, event: PendingConnectionEvent) {
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

    pub(in super::super) async fn close_policy(
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

    pub(in super::super) async fn close_for_connection_limit(&mut self, reason: &'static str) {
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

    pub(in super::super) fn log_summary(&self, request_count: u64) {
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

/// 为 WebSocket transport 启动包含冻结中间件计划的单 owner pump
pub fn spawn_connection<S, E>(
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
    let extensions = crate::middleware::current()
        .map(|context| context.extensions)
        .unwrap_or_default();
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
        extensions,
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
