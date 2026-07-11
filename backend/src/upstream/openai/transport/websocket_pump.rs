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

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::MissedTickBehavior,
};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

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

/// 由后台 pump 任务托管的 WebSocket 连接句柄。
pub(crate) struct PumpedWebSocket {
    tx_command: mpsc::Sender<PumpCommand>,
    rx_message: mpsc::Receiver<Result<Message, tungstenite::Error>>,
    closed: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PumpedWebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PumpedWebSocket")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish()
    }
}

impl PumpedWebSocket {
    /// 用底层流启动一个 pump 任务并返回句柄。
    pub(crate) fn new(inner: RawWsStream, keepalive: PumpKeepalive) -> Self {
        let (tx_command, rx_command) = mpsc::channel::<PumpCommand>(PUMP_COMMAND_BUFFER);
        let (tx_message, rx_message) = mpsc::channel(PUMP_MESSAGE_BUFFER);
        let closed = Arc::new(AtomicBool::new(false));
        let closed_for_task = Arc::clone(&closed);
        let pump = tokio::spawn(async move {
            pump_loop(inner, rx_command, tx_message, keepalive).await;
            closed_for_task.store(true, Ordering::Release);
        });
        Self {
            tx_command,
            rx_message,
            closed,
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

    /// 主动关闭连接（best-effort 发送 Close 帧）。
    pub(crate) async fn close(&mut self) {
        let _ = self.send(Message::Close(None)).await;
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
    mut inner: RawWsStream,
    mut rx_command: mpsc::Receiver<PumpCommand>,
    tx_message: mpsc::Sender<Result<Message, tungstenite::Error>>,
    keepalive: PumpKeepalive,
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

    loop {
        tokio::select! {
            command = rx_command.recv() => {
                match command {
                    // 所有句柄已丢弃：连接不再被使用，退出 pump。
                    None => break,
                    Some(PumpCommand::Send { message, ack }) => {
                        let result = inner.send(message).await;
                        let should_break = result.is_err();
                        let _ = ack.send(result);
                        if should_break {
                            break;
                        }
                    }
                }
            }
            message = inner.next() => {
                match message {
                    None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        last_activity = Instant::now();
                        if inner.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_activity = Instant::now();
                    }
                    Some(Ok(message)) => {
                        last_activity = Instant::now();
                        let is_close = matches!(message, Message::Close(_));
                        if tx_message.try_send(Ok(message)).is_err() {
                            break;
                        }
                        if is_close {
                            break;
                        }
                    }
                    Some(Err(err)) => {
                        let _ = tx_message.try_send(Err(err));
                        break;
                    }
                }
            }
            _ = tick(&mut ticker) => {
                // 先检查 liveness：长时间无入站活动则判定失活退出。
                if let Some(timeout) = liveness_timeout {
                    if last_activity.elapsed() >= timeout {
                        break;
                    }
                }
                // 再按需主动 ping 保活（穿透 NAT/中间盒空闲计时器）。
                if ping_interval.is_some()
                    && inner.send(Message::Ping(Vec::new().into())).await.is_err()
                {
                    break;
                }
            }
        }
    }

    // best-effort：尝试通知对端关闭，随后 drop 底层流释放 socket。
    let _ = inner.send(Message::Close(None)).await;
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
