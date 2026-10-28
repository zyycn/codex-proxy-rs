//! 单 owner 驱动上游 WebSocket，被动回复心跳并在终止后有界释放 socket
//!
//! 业务消息通过有界队列交付，Ping/Pong 由 pump 消费，不刷新生成期间的消息等待超时
//! 入站背压暂停读取，仍可接收关闭命令；已观察到的终止先释放 socket，再等待终止消息交付

use gateway_core::engine::response_control::{ResponseControl, ResponseControlTransport};

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use super::{
    CodexWebSocketCloseError, CodexWebSocketExchangeError,
    control::{PumpCommand, PumpControl, SharedMessages, send_message},
};

/// 底层 tungstenite WebSocket 流
pub(crate) trait WebSocketIo:
    Stream<Item = Result<Message, tungstenite::Error>>
    + Sink<Message, Error = tungstenite::Error>
    + Send
    + Unpin
{
}

impl<T> WebSocketIo for T where
    T: Stream<Item = Result<Message, tungstenite::Error>>
        + Sink<Message, Error = tungstenite::Error>
        + Send
        + Unpin
{
}

pub(crate) type RawWsStream = Box<dyn WebSocketIo>;

const PUMP_COMMAND_BUFFER: usize = 32;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const PONG_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// pump 日志的账号级关联上下文；空闲连接被上游重置等场景用于回溯归属
#[derive(Debug, Clone, Default)]
pub(crate) struct PumpLogContext {
    pub(crate) account_id: Option<String>,
    pub(crate) conversation_id_hash: Option<String>,
}

impl PumpLogContext {
    pub(crate) fn new(account_id: Option<String>, conversation_id_hash: Option<String>) -> Self {
        Self {
            account_id,
            conversation_id_hash,
        }
    }
}

const PUMP_MESSAGE_BUFFER: usize = 64;

#[derive(Debug, Clone)]
pub(crate) enum PumpExitReason {
    CommandChannelClosed,
    LocalClose,
    OutboundTransportError {
        message: String,
        metric_reason: &'static str,
    },
    UpstreamCloseFrame {
        close: Option<CodexWebSocketCloseError>,
    },
    UpstreamEof,
    InboundTransportError {
        message: String,
        metric_reason: &'static str,
    },
    PongWriteTimeout {
        timeout: Duration,
    },
    KeepaliveTransportError {
        message: String,
        metric_reason: &'static str,
    },
    MessageReceiverClosed,
}

impl PumpExitReason {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::CommandChannelClosed => "command_channel_closed",
            Self::LocalClose => "local_close",
            Self::OutboundTransportError { .. } => "outbound_transport_error",
            Self::UpstreamCloseFrame { .. } => "upstream_close_frame",
            Self::UpstreamEof => "upstream_eof",
            Self::InboundTransportError { .. } => "inbound_transport_error",
            Self::PongWriteTimeout { .. } => "pong_write_timeout",
            Self::KeepaliveTransportError { .. } => "keepalive_transport_error",
            Self::MessageReceiverClosed => "message_receiver_closed",
        }
    }

    pub(crate) fn detail(&self) -> Option<String> {
        match self {
            Self::OutboundTransportError { message, .. }
            | Self::InboundTransportError { message, .. }
            | Self::KeepaliveTransportError { message, .. } => Some(message.clone()),
            Self::UpstreamCloseFrame { close } => close.as_ref().map(ToString::to_string),
            Self::PongWriteTimeout { timeout } => Some(format!("{timeout:?}")),
            Self::CommandChannelClosed
            | Self::LocalClose
            | Self::UpstreamEof
            | Self::MessageReceiverClosed => None,
        }
    }

    pub(crate) fn metric_reason(&self) -> &'static str {
        match self {
            Self::CommandChannelClosed => "command_channel_closed",
            Self::LocalClose => "local_close",
            Self::OutboundTransportError { metric_reason, .. }
            | Self::InboundTransportError { metric_reason, .. }
            | Self::KeepaliveTransportError { metric_reason, .. } => metric_reason,
            Self::UpstreamCloseFrame { close } => {
                if close.as_ref().and_then(CodexWebSocketCloseError::code) == Some(1000) {
                    "normal_close"
                } else {
                    "upstream_close"
                }
            }
            Self::UpstreamEof => "eof",
            Self::PongWriteTimeout { .. } => "pong_write_timeout",
            Self::MessageReceiverClosed => "message_receiver_closed",
        }
    }

    pub(crate) fn upstream_close(&self) -> Option<&CodexWebSocketCloseError> {
        match self {
            Self::UpstreamCloseFrame { close: Some(close) } => Some(close),
            _ => None,
        }
    }

    fn is_unexpected(&self) -> bool {
        matches!(
            self,
            Self::OutboundTransportError { .. }
                | Self::UpstreamEof
                | Self::InboundTransportError { .. }
                | Self::PongWriteTimeout { .. }
                | Self::KeepaliveTransportError { .. }
        )
    }

    fn should_send_close(&self) -> bool {
        matches!(
            self,
            Self::CommandChannelClosed
                | Self::UpstreamCloseFrame { .. }
                | Self::MessageReceiverClosed
        )
    }
}

struct PendingInbound {
    item: Result<Message, tungstenite::Error>,
    exit_reason: Option<PumpExitReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSocketConnectionObservation {
    connection_id: Uuid,
    exit_reason: String,
    age_ms: u64,
    idle_ms: u64,
}

impl WebSocketConnectionObservation {
    pub(crate) fn with_exit_reason(mut self, exit_reason: &'static str) -> Self {
        self.exit_reason = exit_reason.to_owned();
        self
    }

    pub const fn connection_id(&self) -> Uuid {
        self.connection_id
    }

    pub fn exit_reason(&self) -> &str {
        &self.exit_reason
    }

    pub const fn age_ms(&self) -> u64 {
        self.age_ms
    }

    pub const fn idle_ms(&self) -> u64 {
        self.idle_ms
    }
}

struct PumpLifecycleState {
    opened_at: Instant,
    last_activity: Instant,
    closed_at: Option<Instant>,
    exit_reason: Option<PumpExitReason>,
}

impl PumpLifecycleState {
    fn observation(
        &self,
        connection_id: Uuid,
        observed_at: Instant,
    ) -> WebSocketConnectionObservation {
        let effective_end = self.closed_at.unwrap_or(observed_at);
        WebSocketConnectionObservation {
            connection_id,
            exit_reason: self
                .exit_reason
                .as_ref()
                .map_or("running", PumpExitReason::metric_reason)
                .to_owned(),
            age_ms: duration_millis(effective_end.duration_since(self.opened_at)),
            idle_ms: duration_millis(effective_end.duration_since(self.last_activity)),
        }
    }
}

/// 由后台 pump 任务托管的 WebSocket 连接句柄
pub(crate) struct PumpedWebSocket {
    connection_id: Uuid,
    tx_command: mpsc::Sender<PumpCommand>,
    rx_message: SharedMessages,
    control: Option<Arc<PumpControl>>,
    closed: Arc<AtomicBool>,
    lifecycle: Arc<Mutex<PumpLifecycleState>>,
    pump: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PumpedWebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PumpedWebSocket")
            .field("connection_id", &self.connection_id)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish()
    }
}

// 字段按声明顺序析构，先释放物理 socket，再归还代理名额
struct PumpSocket {
    stream: RawWsStream,
    _proxy_permit: Option<super::pool::ProxyConnectionPermit>,
}

impl PumpedWebSocket {
    /// 用底层流启动一个 pump 任务并返回句柄
    pub(crate) fn new(
        inner: RawWsStream,
        context: PumpLogContext,
        proxy_permit: Option<super::pool::ProxyConnectionPermit>,
    ) -> Self {
        let (tx_command, rx_command) = mpsc::channel::<PumpCommand>(PUMP_COMMAND_BUFFER);
        let (tx_message, rx_message) = mpsc::channel(PUMP_MESSAGE_BUFFER);
        let connection_id = Uuid::new_v4();
        let opened_at = Instant::now();
        let closed = Arc::new(AtomicBool::new(false));
        let closed_for_task = Arc::clone(&closed);
        let lifecycle = Arc::new(Mutex::new(PumpLifecycleState {
            opened_at,
            last_activity: opened_at,
            closed_at: None,
            exit_reason: None,
        }));
        let lifecycle_for_task = Arc::clone(&lifecycle);
        let inner = PumpSocket {
            stream: inner,
            _proxy_permit: proxy_permit,
        };
        let pump = tokio::spawn(async move {
            pump_loop(
                connection_id,
                inner,
                rx_command,
                tx_message,
                context,
                closed_for_task,
                lifecycle_for_task,
            )
            .await;
        });
        Self {
            connection_id,
            tx_command,
            rx_message: Arc::new(tokio::sync::Mutex::new(rx_message)),
            control: None,
            closed,
            lifecycle,
            pump: Some(pump),
        }
    }

    /// 通过 pump 发送一帧，区分未开始发送与底层传输失败
    pub(crate) async fn send(&self, message: Message) -> Result<(), CodexWebSocketExchangeError> {
        if self.is_closed() {
            return Err(CodexWebSocketExchangeError::SendNotStarted);
        }
        send_message(&self.tx_command, message).await
    }

    /// 每轮发送后替换控制 owner，旧执行的弱引用不能控制下一轮连接
    pub(crate) fn bind_control(&mut self, control: Option<&ResponseControl>) {
        self.revoke_control();
        self.control = control.map(|control| {
            let transport = Arc::new(PumpControl {
                tx_command: self.tx_command.clone(),
                rx_message: Arc::clone(&self.rx_message),
                cancellation: tokio_util::sync::CancellationToken::new(),
            });
            let target: Arc<dyn ResponseControlTransport> = transport.clone();
            control.bind(&target);
            transport
        });
    }

    /// 弱引用升级后的在途读取仍持有强引用，复用前必须主动撤销，释放消息接收锁
    pub(crate) fn revoke_control(&mut self) {
        if let Some(control) = self.control.take() {
            control.cancellation.cancel();
        }
    }

    /// 取出下一帧入站消息（`Ping`/`Pong` 已被 pump 处理，不会到达这里）
    ///
    /// 返回 `None` 表示连接已结束（pump 已退出且缓冲已排空）
    pub(crate) async fn next(&mut self) -> Option<Result<Message, tungstenite::Error>> {
        self.rx_message.lock().await.recv().await
    }

    /// 连接是否已被后台 pump 判定关闭/失活
    /// 零成本，用于复用前探活
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.tx_command.is_closed()
    }

    pub(crate) fn connection_id(&self) -> Uuid {
        self.connection_id
    }

    pub(crate) fn exit_reason(&self) -> Option<PumpExitReason> {
        self.lifecycle
            .lock()
            .expect("WebSocket pump lifecycle lock poisoned")
            .exit_reason
            .clone()
    }

    pub(crate) fn observation(&self) -> WebSocketConnectionObservation {
        self.lifecycle
            .lock()
            .expect("WebSocket pump lifecycle lock poisoned")
            .observation(self.connection_id, Instant::now())
    }

    pub(crate) async fn close_and_wait(self) {
        if !self.is_closed() {
            // 预算覆盖入队和回执，已阻塞的业务写入不能拖住连接回收
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, self.send(Message::Close(None))).await;
        }
        self.abort_and_wait().await;
    }

    /// 异常退出不等待 Close 发送，避免已阻塞的 pump 掩盖原始错误
    pub(crate) async fn abort_and_wait(mut self) {
        if let Some(pump) = self.pump.as_mut() {
            // 只等待任务析构，确保先释放物理 socket，再归还代理名额
            pump.abort();
            let _ = pump.await;
        }
    }
}

impl Drop for PumpedWebSocket {
    fn drop(&mut self) {
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
    }
}

async fn pump_loop(
    connection_id: Uuid,
    mut inner: PumpSocket,
    mut rx_command: mpsc::Receiver<PumpCommand>,
    tx_message: mpsc::Sender<Result<Message, tungstenite::Error>>,
    context: PumpLogContext,
    closed: Arc<AtomicBool>,
    lifecycle: Arc<Mutex<PumpLifecycleState>>,
) {
    let mut pending_inbound = None;
    let mut backpressure_events = 0_u64;

    let reason = 'pump: loop {
        if pending_inbound.is_some() {
            tokio::select! {
                permit = tx_message.reserve() => {
                    let Ok(permit) = permit else {
                        break 'pump PumpExitReason::MessageReceiverClosed;
                    };
                    let pending = pending_inbound
                        .take()
                        .expect("pending inbound frame must exist");
                    permit.send(pending);
                }
                command = rx_command.recv() => {
                    let Some(command) = command else {
                        break 'pump PumpExitReason::CommandChannelClosed;
                    };
                    if let Some(reason) = handle_command(&mut inner.stream, command, closed.load(Ordering::Acquire)).await {
                        break 'pump reason;
                    }
                }
            }
            continue;
        }

        tokio::select! {
            command = rx_command.recv() => {
                let Some(command) = command else {
                    break 'pump PumpExitReason::CommandChannelClosed;
                };
                if let Some(reason) = handle_command(&mut inner.stream, command, closed.load(Ordering::Acquire)).await {
                    break 'pump reason;
                }
            }
            message = inner.stream.next() => {
                match message {
                    None => break 'pump PumpExitReason::UpstreamEof,
                    Some(Ok(message)) => {
                        // Close 是终止信号，不能刷新链路健康时间或抹掉关闭前的静默时长
                        if !matches!(message, Message::Close(_)) {
                            record_activity(&lifecycle, Instant::now());
                        }
                        match message {
                            Message::Ping(payload) => {
                                match tokio::time::timeout(
                                    PONG_WRITE_TIMEOUT,
                                    inner.stream.send(Message::Pong(payload)),
                                ).await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => break 'pump PumpExitReason::KeepaliveTransportError {
                                        message: error.to_string(),
                                        metric_reason: transport_metric_reason(&error),
                                    },
                                    Err(_) => break 'pump PumpExitReason::PongWriteTimeout {
                                        timeout: PONG_WRITE_TIMEOUT,
                                    },
                                }
                            }
                            Message::Pong(_) => {}
                            message => {
                                let terminal_reason = match &message {
                                    Message::Close(frame) => Some(PumpExitReason::UpstreamCloseFrame {
                                        close: frame.as_ref().map(|frame| {
                                            CodexWebSocketCloseError::new(
                                                Some(u16::from(frame.code)),
                                                Some(frame.reason.to_string()),
                                            )
                                        }),
                                    }),
                                    _ => None,
                                };
                                if let Some(reason) = &terminal_reason {
                                    record_closed(&closed, &lifecycle, reason);
                                }
                                if let Some(reason) = enqueue_inbound(
                                    &tx_message,
                                    &mut pending_inbound,
                                    connection_id,
                                    &mut backpressure_events,
                                    PendingInbound {
                                        item: Ok(message),
                                        exit_reason: terminal_reason,
                                    },
                                ) {
                                    break 'pump reason;
                                }
                            }
                        }
                    }
                    Some(Err(err)) => {
                        let terminal_reason = PumpExitReason::InboundTransportError {
                            message: err.to_string(),
                            metric_reason: transport_metric_reason(&err),
                        };
                        record_closed(&closed, &lifecycle, &terminal_reason);
                        if let Some(reason) = enqueue_inbound(
                            &tx_message,
                            &mut pending_inbound,
                            connection_id,
                            &mut backpressure_events,
                            PendingInbound {
                                item: Err(err),
                                exit_reason: Some(terminal_reason),
                            },
                        ) {
                            break 'pump reason;
                        }
                    }
                }
            }
        }
    };

    record_closed(&closed, &lifecycle, &reason);
    // 排队命令尚未进入底层发送，关闭握手前即回绝，避免调用方等待或误判为已发送
    rx_command.close();
    while let Ok(PumpCommand::Send { ack, .. }) = rx_command.try_recv() {
        let _ = ack.send(Err(CodexWebSocketExchangeError::SendNotStarted));
    }
    let observation = {
        let lifecycle = lifecycle
            .lock()
            .expect("WebSocket pump lifecycle lock poisoned");
        lifecycle.observation(connection_id, Instant::now())
    };
    log_pump_exit(
        connection_id,
        &context,
        &reason,
        &observation,
        backpressure_events,
    );

    if reason.should_send_close() {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, inner.stream.send(Message::Close(None))).await;
    }
    // 消费方可能仍在排空业务消息，连接和容量不能继续等它读取终止消息
    drop(inner);
    if let Some(pending) = pending_inbound {
        let _ = tx_message.send(pending).await;
    }
}

fn enqueue_inbound(
    tx_message: &mpsc::Sender<Result<Message, tungstenite::Error>>,
    pending_inbound: &mut Option<Result<Message, tungstenite::Error>>,
    connection_id: Uuid,
    backpressure_events: &mut u64,
    inbound: PendingInbound,
) -> Option<PumpExitReason> {
    let PendingInbound { item, exit_reason } = inbound;
    match tx_message.try_send(item) {
        Ok(()) => exit_reason,
        Err(mpsc::error::TrySendError::Full(item)) => {
            *backpressure_events += 1;
            if *backpressure_events == 1 {
                tracing::info!(
                    websocket_connection_id = %connection_id,
                    buffer_capacity = PUMP_MESSAGE_BUFFER,
                    "Responses WebSocket pump applying inbound backpressure"
                );
            }
            *pending_inbound = Some(item);
            exit_reason
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Some(PumpExitReason::MessageReceiverClosed),
    }
}

async fn handle_command(
    inner: &mut RawWsStream,
    command: PumpCommand,
    closed: bool,
) -> Option<PumpExitReason> {
    let PumpCommand::Send { message, ack } = command;
    // 发送超时或取消后，尚在队列里的业务帧不应继续打到上游
    if ack.is_closed() {
        return None;
    }
    if closed {
        let _ = ack.send(Err(CodexWebSocketExchangeError::SendNotStarted));
        return None;
    }
    let is_close = matches!(message, Message::Close(_));
    let result = inner.send(message).await;
    let transport_error = result
        .as_ref()
        .err()
        .map(|error| (error.to_string(), transport_metric_reason(error)));
    let _ = ack.send(result.map_err(CodexWebSocketExchangeError::Transport));
    match transport_error {
        Some((message, metric_reason)) => Some(PumpExitReason::OutboundTransportError {
            message,
            metric_reason,
        }),
        None if is_close => Some(PumpExitReason::LocalClose),
        None => None,
    }
}

fn record_activity(lifecycle: &Mutex<PumpLifecycleState>, observed_at: Instant) {
    lifecycle
        .lock()
        .expect("WebSocket pump lifecycle lock poisoned")
        .last_activity = observed_at;
}

fn record_closed(
    closed: &AtomicBool,
    lifecycle: &Mutex<PumpLifecycleState>,
    reason: &PumpExitReason,
) {
    let mut lifecycle = lifecycle
        .lock()
        .expect("WebSocket pump lifecycle lock poisoned");
    // 先记录已观察到的终止事实，再通知复用方；入站背压不能延迟连接失效
    if lifecycle.closed_at.is_none() {
        lifecycle.closed_at = Some(Instant::now());
        lifecycle.exit_reason = Some(reason.clone());
    }
    closed.store(true, Ordering::Release);
}

pub(crate) fn transport_metric_reason(error: &tungstenite::Error) -> &'static str {
    match error {
        tungstenite::Error::Io(error) => match error.kind() {
            std::io::ErrorKind::ConnectionReset => "tcp_reset",
            std::io::ErrorKind::ConnectionAborted => "connection_aborted",
            std::io::ErrorKind::BrokenPipe => "broken_pipe",
            std::io::ErrorKind::UnexpectedEof => "unexpected_eof",
            std::io::ErrorKind::TimedOut => "transport_timeout",
            _ => "transport_error",
        },
        // 未收到关闭握手只能证明连接异常结束，不能据此认定收到 TCP RST
        tungstenite::Error::Protocol(
            tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ) => "reset_without_closing_handshake",
        _ => "transport_error",
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn log_pump_exit(
    connection_id: Uuid,
    context: &PumpLogContext,
    reason: &PumpExitReason,
    observation: &WebSocketConnectionObservation,
    backpressure_events: u64,
) {
    let detail = reason.metric_reason();
    let account_id = context.account_id.as_deref().unwrap_or("unknown");
    let conversation_id_hash = context.conversation_id_hash.as_deref().unwrap_or("unknown");
    if reason.is_unexpected() {
        tracing::warn!(
            websocket_connection_id = %connection_id,
            account_id = %account_id,
            conversation_id_hash = %conversation_id_hash,
            pump_exit_reason = reason.as_str(),
            connection_exit_reason = observation.exit_reason(),
            connection_age_ms = observation.age_ms(),
            connection_idle_ms = observation.idle_ms(),
            pump_exit_detail = detail,
            backpressure_events,
            "Responses WebSocket pump stopped unexpectedly"
        );
    } else if matches!(reason, PumpExitReason::UpstreamCloseFrame { .. }) {
        let upstream_close = reason.upstream_close();
        tracing::info!(
            websocket_connection_id = %connection_id,
            account_id = %account_id,
            conversation_id_hash = %conversation_id_hash,
            pump_exit_reason = reason.as_str(),
            connection_exit_reason = observation.exit_reason(),
            connection_age_ms = observation.age_ms(),
            connection_idle_ms = observation.idle_ms(),
            pump_exit_detail = detail,
            upstream_close_code = ?upstream_close.and_then(CodexWebSocketCloseError::code),
            upstream_close_reason_bytes = upstream_close.and_then(CodexWebSocketCloseError::reason).map_or(0, str::len),
            upstream_error_raw_present = upstream_close.is_some_and(|close| close.reason().is_some()),
            backpressure_events,
            "Responses WebSocket pump received close frame"
        );
    } else {
        tracing::debug!(
            websocket_connection_id = %connection_id,
            account_id = %account_id,
            conversation_id_hash = %conversation_id_hash,
            pump_exit_reason = reason.as_str(),
            connection_exit_reason = observation.exit_reason(),
            connection_age_ms = observation.age_ms(),
            connection_idle_ms = observation.idle_ms(),
            pump_exit_detail = detail,
            backpressure_events,
            "Responses WebSocket pump stopped"
        );
    }
}
