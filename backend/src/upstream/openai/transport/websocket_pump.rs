//! 常驻后台的 WebSocket 泵（对齐官方 Codex CLI 的连接管理）。
//!
//! 每条上游 WebSocket 连接都由一个后台 pump 任务独占：
//!   - 持续读取 socket：一旦观察到 `Close` / EOF / 传输错误，立即把连接标记为 `closed`。
//!   - 自动回应上游 `Ping`（保活），并按 `ping_interval` 主动 `Ping`（穿透 NAT/中间盒空闲计时器）。
//!   - 可选 `liveness_timeout`：长时间无任何入站活动时判定连接失活并退出。
//!
//! 因此空闲连接的“死没死”在后台被实时感知；复用方只需零成本读取 [`PumpedWebSocket::is_closed`]，
//! 无需在请求路径上做同步探活 ping（消除“复用到静默死连接才卡住超时”的长尾）。
//!
//! 收发都通过 channel 与 pump 任务交互：
//!   - 发送：`send` 走 command channel，等待 pump 回执。
//!   - 接收：`next` 从 message channel 取出 pump 转发的入站帧（`Ping`/`Pong` 已被 pump 吞掉）。
//!   - 入站缓冲满时暂停读取 socket，并继续处理关闭命令与保活；消费恢复后按原顺序继续转发。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use uuid::Uuid;

/// 底层 tungstenite WebSocket 流。
pub(crate) type RawWsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

const PUMP_COMMAND_BUFFER: usize = 32;

/// pump 保活策略。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PumpKeepalive {
    /// 主动 `Ping` 间隔；`None` 表示 pump 不主动 ping（仅被动读取 + 回应上游 ping）。
    pub(crate) ping_interval: Option<Duration>,
    /// 无入站活动多久后判定失活；`None` 表示只靠显式 close/传输错误发现死亡。
    pub(crate) liveness_timeout: Option<Duration>,
}

impl PumpKeepalive {
    /// 不做主动保活（用于即用即弃的非池化连接）。
    pub(crate) fn disabled() -> Self {
        Self {
            ping_interval: None,
            liveness_timeout: None,
        }
    }
}

enum PumpCommand {
    Send {
        message: Message,
        ack: oneshot::Sender<Result<(), tungstenite::Error>>,
    },
}

const PUMP_MESSAGE_BUFFER: usize = 64;

#[derive(Debug, Clone)]
pub(crate) enum PumpExitReason {
    CommandChannelClosed,
    LocalClose,
    OutboundTransportError { message: String },
    UpstreamCloseFrame { frame: Option<String> },
    UpstreamEof,
    InboundTransportError { message: String },
    LivenessTimeout { timeout: Duration },
    KeepaliveTransportError { message: String },
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
            Self::LivenessTimeout { .. } => "liveness_timeout",
            Self::KeepaliveTransportError { .. } => "keepalive_transport_error",
            Self::MessageReceiverClosed => "message_receiver_closed",
        }
    }

    pub(crate) fn detail(&self) -> Option<String> {
        match self {
            Self::OutboundTransportError { message }
            | Self::InboundTransportError { message }
            | Self::KeepaliveTransportError { message } => Some(message.clone()),
            Self::UpstreamCloseFrame { frame } => frame.clone(),
            Self::LivenessTimeout { timeout } => Some(format!("{timeout:?}")),
            Self::CommandChannelClosed
            | Self::LocalClose
            | Self::UpstreamEof
            | Self::MessageReceiverClosed => None,
        }
    }

    fn is_unexpected(&self) -> bool {
        matches!(
            self,
            Self::OutboundTransportError { .. }
                | Self::UpstreamEof
                | Self::InboundTransportError { .. }
                | Self::LivenessTimeout { .. }
                | Self::KeepaliveTransportError { .. }
        )
    }

    fn should_send_close(&self) -> bool {
        matches!(
            self,
            Self::CommandChannelClosed
                | Self::UpstreamCloseFrame { .. }
                | Self::LivenessTimeout { .. }
                | Self::MessageReceiverClosed
        )
    }
}

struct PendingInbound {
    item: Result<Message, tungstenite::Error>,
    exit_reason: Option<PumpExitReason>,
}

/// 由后台 pump 任务托管的 WebSocket 连接句柄。
pub(crate) struct PumpedWebSocket {
    connection_id: Uuid,
    tx_command: mpsc::Sender<PumpCommand>,
    rx_message: mpsc::Receiver<Result<Message, tungstenite::Error>>,
    closed: Arc<AtomicBool>,
    exit_reason: Arc<Mutex<Option<PumpExitReason>>>,
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

impl PumpedWebSocket {
    /// 用底层流启动一个 pump 任务并返回句柄。
    pub(crate) fn new(inner: RawWsStream, keepalive: PumpKeepalive) -> Self {
        let (tx_command, rx_command) = mpsc::channel::<PumpCommand>(PUMP_COMMAND_BUFFER);
        let (tx_message, rx_message) = mpsc::channel(PUMP_MESSAGE_BUFFER);
        let connection_id = Uuid::new_v4();
        let closed = Arc::new(AtomicBool::new(false));
        let closed_for_task = Arc::clone(&closed);
        let exit_reason = Arc::new(Mutex::new(None));
        let exit_reason_for_task = Arc::clone(&exit_reason);
        let pump = tokio::spawn(async move {
            pump_loop(
                connection_id,
                inner,
                rx_command,
                tx_message,
                keepalive,
                closed_for_task,
                exit_reason_for_task,
            )
            .await;
        });
        Self {
            connection_id,
            tx_command,
            rx_message,
            closed,
            exit_reason,
            pump: Some(pump),
        }
    }

    /// 通过 pump 发送一帧，返回底层 `send` 的结果。
    pub(crate) async fn send(&self, message: Message) -> Result<(), tungstenite::Error> {
        let (ack, rx_ack) = oneshot::channel();
        if self
            .tx_command
            .send(PumpCommand::Send { message, ack })
            .await
            .is_err()
        {
            return Err(tungstenite::Error::ConnectionClosed);
        }
        rx_ack
            .await
            .unwrap_or(Err(tungstenite::Error::ConnectionClosed))
    }

    /// 取出下一帧入站消息（`Ping`/`Pong` 已被 pump 处理，不会到达这里）。
    ///
    /// 返回 `None` 表示连接已结束（pump 已退出且缓冲已排空）。
    pub(crate) async fn next(&mut self) -> Option<Result<Message, tungstenite::Error>> {
        self.rx_message.recv().await
    }

    /// 连接是否已被后台 pump 判定关闭/失活。零成本，用于复用前探活。
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.tx_command.is_closed()
    }

    pub(crate) fn connection_id(&self) -> Uuid {
        self.connection_id
    }

    pub(crate) fn exit_reason(&self) -> Option<PumpExitReason> {
        self.exit_reason
            .lock()
            .expect("WebSocket pump exit reason lock poisoned")
            .clone()
    }

    /// 主动关闭连接（best-effort 发送 Close 帧）。
    pub(crate) async fn close(&self) {
        if !self.is_closed() {
            let _ = self.send(Message::Close(None)).await;
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
    mut inner: RawWsStream,
    mut rx_command: mpsc::Receiver<PumpCommand>,
    tx_message: mpsc::Sender<Result<Message, tungstenite::Error>>,
    keepalive: PumpKeepalive,
    closed: Arc<AtomicBool>,
    exit_reason: Arc<Mutex<Option<PumpExitReason>>>,
) {
    let mut last_activity = Instant::now();
    let ping_interval = keepalive.ping_interval.filter(|d| !d.is_zero());
    let liveness_timeout = keepalive.liveness_timeout.filter(|d| !d.is_zero());
    // ticker 由 ping 间隔或 liveness 检查间隔中较小者驱动：即使不主动 ping，
    // 也要周期性醒来检查 liveness。两者都未配置时不启动 ticker。
    let tick_interval = match (ping_interval, liveness_timeout) {
        (Some(ping), Some(live)) => Some(ping.min(live)),
        (Some(ping), None) => Some(ping),
        (None, Some(live)) => Some(live),
        (None, None) => None,
    };
    let mut ticker = tick_interval.map(|d| {
        // 首个 tick 推迟一整个间隔：tokio::time::interval 默认会让首个 tick 立即就绪，
        // 否则连接一建立就会立刻发一帧 Ping，与首个请求 Text 抢跑、打乱帧序。
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + d, d);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker
    });

    let mut pending_inbound: Option<PendingInbound> = None;
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
                    permit.send(pending.item);
                    if let Some(reason) = pending.exit_reason {
                        break 'pump reason;
                    }
                }
                command = rx_command.recv() => {
                    let Some(command) = command else {
                        break 'pump PumpExitReason::CommandChannelClosed;
                    };
                    if let Some(reason) = handle_command(&mut inner, command).await {
                        break 'pump reason;
                    }
                }
                _ = tick(&mut ticker) => {
                    if let Some(reason) = handle_tick(
                        &mut inner,
                        last_activity,
                        ping_interval,
                        liveness_timeout,
                        false,
                    ).await {
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
                if let Some(reason) = handle_command(&mut inner, command).await {
                    break 'pump reason;
                }
            }
            message = inner.next() => {
                match message {
                    None => break 'pump PumpExitReason::UpstreamEof,
                    Some(Ok(Message::Ping(payload))) => {
                        last_activity = Instant::now();
                        if let Err(error) = inner.send(Message::Pong(payload)).await {
                            break 'pump PumpExitReason::KeepaliveTransportError {
                                message: error.to_string(),
                            };
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_activity = Instant::now();
                    }
                    Some(Ok(message)) => {
                        last_activity = Instant::now();
                        let terminal_reason = match &message {
                            Message::Close(frame) => Some(PumpExitReason::UpstreamCloseFrame {
                                frame: frame.as_ref().map(|frame| format!("{frame:?}")),
                            }),
                            _ => None,
                        };
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
                    Some(Err(err)) => {
                        let terminal_reason = PumpExitReason::InboundTransportError {
                            message: err.to_string(),
                        };
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
            _ = tick(&mut ticker) => {
                if let Some(reason) = handle_tick(
                    &mut inner,
                    last_activity,
                    ping_interval,
                    liveness_timeout,
                    true,
                ).await {
                    break 'pump reason;
                }
            }
        }
    };

    closed.store(true, Ordering::Release);
    *exit_reason
        .lock()
        .expect("WebSocket pump exit reason lock poisoned") = Some(reason.clone());
    log_pump_exit(connection_id, &reason, backpressure_events);

    if reason.should_send_close() {
        let _ = inner.send(Message::Close(None)).await;
    }
}

fn enqueue_inbound(
    tx_message: &mpsc::Sender<Result<Message, tungstenite::Error>>,
    pending_inbound: &mut Option<PendingInbound>,
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
            *pending_inbound = Some(PendingInbound { item, exit_reason });
            None
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Some(PumpExitReason::MessageReceiverClosed),
    }
}

async fn handle_command(inner: &mut RawWsStream, command: PumpCommand) -> Option<PumpExitReason> {
    let PumpCommand::Send { message, ack } = command;
    let is_close = matches!(message, Message::Close(_));
    let result = inner.send(message).await;
    let transport_error = result.as_ref().err().map(ToString::to_string);
    let _ = ack.send(result);
    match transport_error {
        Some(message) => Some(PumpExitReason::OutboundTransportError { message }),
        None if is_close => Some(PumpExitReason::LocalClose),
        None => None,
    }
}

async fn handle_tick(
    inner: &mut RawWsStream,
    last_activity: Instant,
    ping_interval: Option<Duration>,
    liveness_timeout: Option<Duration>,
    check_liveness: bool,
) -> Option<PumpExitReason> {
    if check_liveness
        && let Some(timeout) = liveness_timeout
        && last_activity.elapsed() >= timeout
    {
        return Some(PumpExitReason::LivenessTimeout { timeout });
    }
    if ping_interval.is_some()
        && let Err(error) = inner.send(Message::Ping(Vec::new().into())).await
    {
        return Some(PumpExitReason::KeepaliveTransportError {
            message: error.to_string(),
        });
    }
    None
}

fn log_pump_exit(connection_id: Uuid, reason: &PumpExitReason, backpressure_events: u64) {
    let detail = reason.detail().unwrap_or_default();
    if reason.is_unexpected() {
        tracing::warn!(
            websocket_connection_id = %connection_id,
            pump_exit_reason = reason.as_str(),
            pump_exit_detail = detail,
            backpressure_events,
            "Responses WebSocket pump stopped unexpectedly"
        );
    } else if matches!(reason, PumpExitReason::UpstreamCloseFrame { .. }) {
        tracing::info!(
            websocket_connection_id = %connection_id,
            pump_exit_reason = reason.as_str(),
            pump_exit_detail = detail,
            backpressure_events,
            "Responses WebSocket pump received close frame"
        );
    } else {
        tracing::debug!(
            websocket_connection_id = %connection_id,
            pump_exit_reason = reason.as_str(),
            pump_exit_detail = detail,
            backpressure_events,
            "Responses WebSocket pump stopped"
        );
    }
}

/// 等待 ping ticker；`None` 时永远挂起，让 `select!` 分支实际禁用。
async fn tick(ticker: &mut Option<tokio::time::Interval>) {
    match ticker {
        Some(ticker) => {
            ticker.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}
