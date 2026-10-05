//! 插件侧双向 RPC 会话的握手、调用分派、流控与取消生命周期

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

use crate::{
    CallContext, ErrorCode, Frame, FrameError, Handshake, Message, PROTOCOL_VERSION, PluginFault,
};

use super::frame::{read_frame, validate_frame, write_frame};

const MAXIMUM_STREAM_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_CONCURRENCY: usize = 256;
const MAXIMUM_BUFFERED_STREAM_CHUNKS: usize = 65_536;

/// 插件侧会话的内存、并发与期限边界
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    /// 单个业务流分块的预算，不限制普通调用正文的总长度
    pub maximum_stream_chunk_bytes: usize,
    pub maximum_calls: usize,
    pub maximum_callbacks: usize,
    pub maximum_buffered_stream_chunks: usize,
    pub handshake_timeout: Duration,
    pub maximum_call_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            // 流分块还需匹配宿主授予的信用窗口
            maximum_stream_chunk_bytes: 1024 * 1024,
            maximum_calls: 32,
            maximum_callbacks: 32,
            maximum_buffered_stream_chunks: 1_024,
            handshake_timeout: Duration::from_secs(5),
            maximum_call_timeout: Duration::from_secs(120),
        }
    }
}

impl SessionConfig {
    fn validate(self) -> Result<Self, SessionError> {
        if self.maximum_stream_chunk_bytes < 1_024
            || self.maximum_stream_chunk_bytes > MAXIMUM_STREAM_CHUNK_BYTES
            || self.maximum_calls == 0
            || self.maximum_calls > MAXIMUM_CONCURRENCY
            || self.maximum_callbacks == 0
            || self.maximum_callbacks > MAXIMUM_CONCURRENCY
            || self.maximum_buffered_stream_chunks == 0
            || self.maximum_buffered_stream_chunks > MAXIMUM_BUFFERED_STREAM_CHUNKS
            || self.handshake_timeout.is_zero()
            || self.maximum_call_timeout.is_zero()
        {
            return Err(SessionError::Configuration);
        }
        Ok(self)
    }
}

/// 插件侧传输和会话错误
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("plugin session configuration is invalid")]
    Configuration,
    #[error("plugin handshake is invalid")]
    Handshake,
    #[error("plugin protocol is invalid")]
    Protocol,
    #[error("plugin transport is closed")]
    Closed,
    #[error("plugin operation exceeded its deadline")]
    Timeout,
    #[error("plugin call was cancelled")]
    Cancelled,
    #[error("plugin session capacity is exhausted")]
    Capacity,
    #[error("plugin handler stopped unexpectedly")]
    HandlerStopped,
    #[error("host callback returned an error: {0:?}")]
    Remote(PluginFault),
    #[error(transparent)]
    Frame(#[from] FrameError),
}

impl SessionError {
    /// 将本地会话失败收敛为可返回宿主的稳定插件错误
    #[must_use]
    pub fn into_plugin_fault(self) -> PluginFault {
        match self {
            Self::Remote(fault) => fault,
            Self::Timeout => PluginFault::new(ErrorCode::Timeout, "host callback timed out"),
            Self::Cancelled => PluginFault::new(ErrorCode::Cancelled, "parent call was cancelled"),
            Self::Capacity => PluginFault::new(ErrorCode::Capacity, "plugin capacity is exhausted"),
            Self::Configuration | Self::Protocol | Self::Handshake | Self::Frame(_) => {
                PluginFault::new(ErrorCode::InvalidInput, "plugin session input is invalid")
            }
            Self::Closed | Self::HandlerStopped => {
                PluginFault::new(ErrorCode::Fault, "plugin session is unavailable")
            }
        }
    }
}

/// 一次宿主调用的可克隆取消信号
#[derive(Clone)]
pub struct CallCancellation {
    receiver: watch::Receiver<bool>,
}

impl CallCancellation {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        if *receiver.borrow() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }
}

struct CancellationSource {
    sender: watch::Sender<bool>,
}

impl CancellationSource {
    fn new() -> (Self, CallCancellation) {
        let (sender, receiver) = watch::channel(false);
        (Self { sender }, CallCancellation { receiver })
    }

    fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

/// 宿主回调的成功结果；不实现 `Debug`，避免载荷进入普通诊断
pub struct HostReply {
    pub result: Value,
    pub payload: Vec<u8>,
}

/// 绑定父调用、期限和取消信号的宿主回调客户端
#[derive(Clone)]
pub struct HostClient {
    parent_id: u64,
    deadline: tokio::sync::watch::Receiver<Option<Instant>>,
    cancellation: CallCancellation,
    callbacks: Arc<CallbackRegistry>,
    output: Output,
    maximum_stream_chunk_bytes: usize,
    credits: Arc<CreditWindow>,
}

impl HostClient {
    pub(crate) fn maximum_stream_chunk_bytes(&self) -> usize {
        let window = lock_unpoisoned(&self.credits.state).maximum_bytes;
        if window == 0 {
            self.maximum_stream_chunk_bytes
        } else {
            self.maximum_stream_chunk_bytes
                .min(usize::try_from(window).unwrap_or(usize::MAX))
        }
    }

    /// 发起一次与父调用关联的宿主回调
    ///
    /// # Errors
    ///
    /// 方法或帧无效、容量耗尽、父调用取消、期限到达、连接关闭或宿主返回错误时失败
    pub async fn call(
        &self,
        method: impl Into<String>,
        params: Value,
        payload: Vec<u8>,
    ) -> Result<HostReply, SessionError> {
        let method = method.into();
        if method.is_empty() || method.len() > 128 {
            return Err(SessionError::Protocol);
        }
        let ordered = self.callbacks.order.lock().await;
        let (id, received) = self.callbacks.begin(self.parent_id)?;
        let frame = Frame {
            message: Message::Callback {
                id,
                parent_id: self.parent_id,
                method,
                params,
            },
            payload,
        };
        if validate_frame(&frame).is_err() {
            self.callbacks.retire(id);
            return Err(SessionError::Protocol);
        }
        let sent = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(SessionError::Cancelled),
            () = callback_deadline(self.deadline.clone()) => Err(SessionError::Timeout),
            result = self.output.send_data(frame) => result,
        };
        if let Err(error) = sent {
            self.callbacks.retire(id);
            return Err(error);
        }
        drop(ordered);
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                self.callbacks.retire(id);
                Err(SessionError::Cancelled)
            }
            () = callback_deadline(self.deadline.clone()) => {
                self.callbacks.retire(id);
                Err(SessionError::Timeout)
            }
            result = received => result.map_err(|_| SessionError::Closed)?,
        }
    }
}

/// 交给插件业务处理器的一次宿主调用；不实现 `Debug`，避免敏感载荷泄露
pub struct PluginCall {
    pub method: String,
    pub context: CallContext,
    pub params: Value,
    pub payload: Vec<u8>,
    pub host: HostClient,
    pub cancellation: CallCancellation,
}

/// 插件业务调用 future
pub type CallFuture<'a> = Pin<Box<dyn Future<Output = Result<CallReply, PluginFault>> + Send + 'a>>;

/// 插件只实现业务调用；会话负责握手、关联、流控、取消和关闭
pub trait PluginHandler: Send + Sync + 'static {
    fn call(&self, call: PluginCall) -> CallFuture<'_>;

    /// 取消时释放只属于该调用的临时业务状态
    /// 实现必须幂等、快速且不得执行阻塞 I/O
    fn cancel(&self, _context: &CallContext) {}

    /// 会话停止接收新调用时触发
    /// 实现必须幂等、快速且不得执行阻塞 I/O
    fn quiesce(&self) {}

    /// 会话关闭时释放插件自有临时状态
    /// 实现必须幂等、快速且不得执行阻塞 I/O
    fn shutdown(&self) {}
}

/// 一次业务成功结果；`stream` 存在时 SDK 自动产生单一流终态
pub struct CallReply {
    result: Value,
    payload: Vec<u8>,
    stream: Option<ResponseStream>,
}

impl CallReply {
    #[must_use]
    pub fn unary(result: Value, payload: Vec<u8>) -> Self {
        Self {
            result,
            payload,
            stream: None,
        }
    }

    #[must_use]
    pub fn stream(result: Value, payload: Vec<u8>, stream: ResponseStream) -> Self {
        Self {
            result,
            payload,
            stream: Some(stream),
        }
    }
}

enum StreamSource {
    Buffered(VecDeque<Vec<u8>>),
    Channel(mpsc::Receiver<Result<Vec<u8>, PluginFault>>),
    Pull(Box<dyn PullResponseStream>),
}

pub type PullResponseFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Result<Vec<u8>, PluginFault>>> + Send + 'a>>;

pub trait PullResponseStream: Send {
    fn next(&mut self) -> PullResponseFuture<'_>;
}

/// SDK 管理 sequence、Credit 和终态的响应流
pub struct ResponseStream {
    source: StreamSource,
    declared_capacity: usize,
}

impl ResponseStream {
    /// 从已完成业务校验的有限分块构造流；SDK 会在发送 `Result` 前预检全部分块
    #[must_use]
    pub fn from_chunks(chunks: Vec<Vec<u8>>) -> Self {
        let declared_capacity = chunks.len().max(1);
        Self {
            source: StreamSource::Buffered(chunks.into()),
            declared_capacity,
        }
    }

    /// 创建由生产者驱动的有界流队列
    #[must_use]
    pub fn channel(capacity: NonZeroUsize) -> (StreamSender, Self) {
        let (sender, receiver) = mpsc::channel(capacity.get());
        (
            StreamSender { sender },
            Self {
                source: StreamSource::Channel(receiver),
                declared_capacity: capacity.get(),
            },
        )
    }

    /// 按消费进度拉取下一帧；返回 None 表示完成，丢弃流会丢弃生产者
    pub fn pull(source: Box<dyn PullResponseStream>) -> Self {
        Self {
            source: StreamSource::Pull(source),
            declared_capacity: 1,
        }
    }

    fn validate_buffered(
        &self,
        maximum_chunks: usize,
        maximum_stream_chunk_bytes: usize,
        window_bytes: u64,
    ) -> Result<(), PluginFault> {
        if self.declared_capacity > maximum_chunks {
            return Err(capacity_fault(
                "response stream exceeds the local queue limit",
            ));
        }
        if let StreamSource::Buffered(chunks) = &self.source {
            for chunk in chunks {
                validate_stream_chunk(chunk, maximum_stream_chunk_bytes, window_bytes)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn next(&mut self) -> Option<Result<Vec<u8>, PluginFault>> {
        match &mut self.source {
            StreamSource::Buffered(chunks) => chunks.pop_front().map(Ok),
            StreamSource::Channel(receiver) => receiver.recv().await,
            StreamSource::Pull(source) => source.next().await,
        }
    }
}

/// 动态响应流的有界生产端；最后一个 sender 被释放表示成功终态
#[derive(Clone)]
pub struct StreamSender {
    sender: mpsc::Sender<Result<Vec<u8>, PluginFault>>,
}

impl StreamSender {
    /// 排队一个业务分块；实际线协议流控由会话完成
    pub async fn send(&self, payload: Vec<u8>) -> Result<(), SessionError> {
        self.sender
            .send(Ok(payload))
            .await
            .map_err(|_| SessionError::Closed)
    }

    /// 以业务错误结束动态流
    pub async fn fail(&self, fault: PluginFault) -> Result<(), SessionError> {
        self.sender
            .send(Err(fault))
            .await
            .map_err(|_| SessionError::Closed)
    }
}

/// 已读取并校验 Hello、尚未向宿主发送 Ready 的插件会话
pub struct PluginSession<R, W> {
    reader: R,
    writer: W,
    handshake: Handshake,
    config: SessionConfig,
}

impl<R, W> PluginSession<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    /// 读取并校验宿主握手
    /// 业务处理器可以在 `run` 前读取不可变 Handshake 构造注册描述
    ///
    /// # Errors
    ///
    /// 配置无效、握手超时、传输关闭或 Hello 不符合协议时失败
    pub async fn accept(
        mut reader: R,
        writer: W,
        config: SessionConfig,
    ) -> Result<Self, SessionError> {
        let config = config.validate()?;
        let frame = tokio::time::timeout(config.handshake_timeout, read_frame(&mut reader))
            .await
            .map_err(|_| SessionError::Timeout)??;
        let Message::Hello { handshake } = frame.message else {
            return Err(SessionError::Handshake);
        };
        if !frame.payload.is_empty()
            || handshake.protocol_version != PROTOCOL_VERSION
            || handshake.plugin_id.is_empty()
            || handshake.instance_id.is_empty()
            || handshake.generation == 0
            || handshake.incarnation.is_empty()
        {
            return Err(SessionError::Handshake);
        }
        Ok(Self {
            reader,
            writer,
            handshake,
            config,
        })
    }

    #[must_use]
    pub const fn handshake(&self) -> &Handshake {
        &self.handshake
    }

    /// 发送 Ready 并持续处理调用、回调、流控、取消与关闭
    ///
    /// # Errors
    ///
    /// 帧损坏、协议违规、处理器异常停止或传输关闭时失败
    pub async fn run<H>(mut self, handler: H) -> Result<(), SessionError>
    where
        H: PluginHandler,
    {
        let data_capacity = self
            .config
            .maximum_calls
            .checked_add(self.config.maximum_callbacks)
            .ok_or(SessionError::Configuration)?;
        let control_reserve = self
            .config
            .maximum_calls
            .checked_add(8)
            .ok_or(SessionError::Configuration)?;
        let queue_capacity = data_capacity
            .checked_add(control_reserve)
            .ok_or(SessionError::Configuration)?;
        let (outbound, received) = mpsc::channel(queue_capacity);
        let output = Output {
            sender: outbound,
            data_slots: Arc::new(Semaphore::new(data_capacity)),
        };
        let callbacks = Arc::new(CallbackRegistry::new(self.config.maximum_callbacks));
        let handler: Arc<dyn PluginHandler> = Arc::new(handler);
        let lifecycle = LifecycleHooks::new(Arc::clone(&handler), control_reserve)?;
        write_frame(
            &mut self.writer,
            &Frame::control(Message::Ready {
                protocol_version: PROTOCOL_VERSION,
                incarnation: self.handshake.incarnation.clone(),
            }),
        )
        .await?;
        let writer = tokio::spawn(writer_loop(self.writer, received));
        let (completed, completions) = mpsc::channel(self.config.maximum_calls);
        let mut driver = SessionDriver {
            config: self.config,
            instance_id: self.handshake.instance_id,
            generation: self.handshake.generation,
            incarnation: self.handshake.incarnation,
            output,
            callbacks,
            handler,
            lifecycle,
            writer,
            completed,
            completions,
            active: BTreeMap::new(),
            last_call: 0,
            quiescing: false,
        };
        let result = driver.drive(self.reader).await;
        driver.close().await;
        result
    }
}

struct Output {
    sender: mpsc::Sender<Outbound>,
    data_slots: Arc<Semaphore>,
}

impl Clone for Output {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            data_slots: Arc::clone(&self.data_slots),
        }
    }
}

impl Output {
    async fn send_data(&self, frame: Frame) -> Result<(), SessionError> {
        validate_frame(&frame)?;
        let permit = Arc::clone(&self.data_slots)
            .acquire_owned()
            .await
            .map_err(|_| SessionError::Closed)?;
        self.sender
            .send(Outbound {
                frame,
                _data_slot: Some(permit),
            })
            .await
            .map_err(|_| SessionError::Closed)
    }

    async fn send_data_until(
        &self,
        frame: Frame,
        deadline: Option<Instant>,
        cancellation: &CallCancellation,
    ) -> Result<(), SessionError> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(SessionError::Cancelled),
            () = wait_deadline(deadline) => Err(SessionError::Timeout),
            result = self.send_data(frame) => result,
        }
    }

    fn send_control(&self, frame: Frame) -> Result<(), SessionError> {
        validate_frame(&frame)?;
        self.sender
            .try_send(Outbound {
                frame,
                _data_slot: None,
            })
            .map_err(|_| SessionError::Capacity)
    }
}

struct Outbound {
    frame: Frame,
    _data_slot: Option<OwnedSemaphorePermit>,
}

async fn writer_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut received: mpsc::Receiver<Outbound>,
) -> Result<(), SessionError> {
    while let Some(outbound) = received.recv().await {
        write_frame(&mut writer, &outbound.frame).await?;
    }
    Ok(())
}

struct CallbackRegistry {
    state: Mutex<CallbackState>,
    order: tokio::sync::Mutex<()>,
    maximum: usize,
    maximum_retired: usize,
}

struct CallbackState {
    next_id: u64,
    pending: BTreeMap<u64, PendingCallback>,
    retired: BTreeSet<u64>,
    retired_order: VecDeque<u64>,
}

struct PendingCallback {
    parent_id: u64,
    result: oneshot::Sender<Result<HostReply, SessionError>>,
}

impl CallbackRegistry {
    fn new(maximum: usize) -> Self {
        Self {
            state: Mutex::new(CallbackState {
                next_id: 2,
                pending: BTreeMap::new(),
                retired: BTreeSet::new(),
                retired_order: VecDeque::new(),
            }),
            order: tokio::sync::Mutex::new(()),
            maximum,
            maximum_retired: maximum.saturating_mul(2),
        }
    }

    fn begin(
        &self,
        parent_id: u64,
    ) -> Result<(u64, oneshot::Receiver<Result<HostReply, SessionError>>), SessionError> {
        let mut state = lock_unpoisoned(&self.state);
        if state.pending.len() >= self.maximum {
            return Err(SessionError::Capacity);
        }
        let id = state.next_id;
        state.next_id = state.next_id.checked_add(2).ok_or(SessionError::Capacity)?;
        let (result, received) = oneshot::channel();
        state
            .pending
            .insert(id, PendingCallback { parent_id, result });
        Ok((id, received))
    }

    fn resolve(&self, frame: Frame) -> Result<(), SessionError> {
        let id = match &frame.message {
            Message::Result { id, .. } | Message::Error { id, .. } => *id,
            _ => return Err(SessionError::Protocol),
        };
        let pending = {
            let mut state = lock_unpoisoned(&self.state);
            if let Some(pending) = state.pending.remove(&id) {
                Some(pending)
            } else if state.retired.remove(&id) {
                state.retired_order.retain(|retired| *retired != id);
                None
            } else {
                return Err(SessionError::Protocol);
            }
        };
        let Some(pending) = pending else {
            return Ok(());
        };
        let result = match frame.message {
            Message::Result { result, .. } => Ok(HostReply {
                result,
                payload: frame.payload,
            }),
            Message::Error { error, .. } if frame.payload.is_empty() => {
                Err(SessionError::Remote(error))
            }
            _ => return Err(SessionError::Protocol),
        };
        let _ = pending.result.send(result);
        Ok(())
    }

    fn retire(&self, id: u64) {
        let mut state = lock_unpoisoned(&self.state);
        if state.pending.remove(&id).is_some() {
            retire_callback(&mut state, id, self.maximum_retired);
        }
    }

    fn finish_parent(&self, parent_id: u64) {
        let pending = {
            let mut state = lock_unpoisoned(&self.state);
            let mut remaining = BTreeMap::new();
            let mut finished = Vec::new();
            for (id, pending) in std::mem::take(&mut state.pending) {
                if pending.parent_id == parent_id {
                    retire_callback(&mut state, id, self.maximum_retired);
                    finished.push(pending);
                } else {
                    remaining.insert(id, pending);
                }
            }
            state.pending = remaining;
            finished
        };
        for pending in pending {
            let _ = pending.result.send(Err(SessionError::Cancelled));
        }
    }

    fn close(&self) {
        let pending = {
            let mut state = lock_unpoisoned(&self.state);
            std::mem::take(&mut state.pending)
        };
        for pending in pending.into_values() {
            let _ = pending.result.send(Err(SessionError::Closed));
        }
    }
}

fn retire_callback(state: &mut CallbackState, id: u64, maximum: usize) {
    if state.retired.insert(id) {
        state.retired_order.push_back(id);
    }
    while state.retired_order.len() > maximum {
        if let Some(expired) = state.retired_order.pop_front() {
            state.retired.remove(&expired);
        }
    }
}

enum LifecycleEvent {
    Cancel(CallContext),
    Quiesce,
    Shutdown,
}

#[derive(Clone)]
struct LifecycleHooks {
    events: std::sync::mpsc::SyncSender<LifecycleEvent>,
}

impl LifecycleHooks {
    fn new(handler: Arc<dyn PluginHandler>, capacity: usize) -> Result<Self, SessionError> {
        let (events, received) = std::sync::mpsc::sync_channel(capacity);
        std::thread::Builder::new()
            .name("gateway-plugin-sdk-lifecycle".into())
            .spawn(move || {
                while let Ok(event) = received.recv() {
                    match event {
                        LifecycleEvent::Cancel(context) => handler.cancel(&context),
                        LifecycleEvent::Quiesce => handler.quiesce(),
                        LifecycleEvent::Shutdown => {
                            handler.shutdown();
                            break;
                        }
                    }
                }
            })
            .map_err(|_| SessionError::Closed)?;
        Ok(Self { events })
    }

    fn cancel(&self, context: CallContext) {
        let _ = self.events.try_send(LifecycleEvent::Cancel(context));
    }

    fn quiesce(&self) {
        let _ = self.events.try_send(LifecycleEvent::Quiesce);
    }

    fn shutdown(&self) {
        let _ = self.events.try_send(LifecycleEvent::Shutdown);
    }
}

struct ActiveCall {
    context: CallContext,
    credits: Arc<CreditWindow>,
    cancellation: CancellationSource,
    task: JoinHandle<()>,
}

#[derive(Clone, Copy)]
enum CallTaskOutcome {
    Complete,
    TransportFailed,
    HandlerStopped,
}

struct CallFinished {
    id: u64,
    outcome: CallTaskOutcome,
}

struct CompletionGuard {
    id: u64,
    completed: mpsc::Sender<CallFinished>,
    armed: bool,
}

impl CompletionGuard {
    fn new(id: u64, completed: mpsc::Sender<CallFinished>) -> Self {
        Self {
            id,
            completed,
            armed: true,
        }
    }

    fn finish(mut self, outcome: CallTaskOutcome) {
        self.armed = false;
        let _ = self.completed.try_send(CallFinished {
            id: self.id,
            outcome,
        });
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.completed.try_send(CallFinished {
                id: self.id,
                outcome: CallTaskOutcome::HandlerStopped,
            });
        }
    }
}

struct SessionDriver {
    config: SessionConfig,
    instance_id: String,
    generation: u64,
    incarnation: String,
    output: Output,
    callbacks: Arc<CallbackRegistry>,
    handler: Arc<dyn PluginHandler>,
    lifecycle: LifecycleHooks,
    writer: JoinHandle<Result<(), SessionError>>,
    completed: mpsc::Sender<CallFinished>,
    completions: mpsc::Receiver<CallFinished>,
    active: BTreeMap<u64, ActiveCall>,
    last_call: u64,
    quiescing: bool,
}

impl SessionDriver {
    async fn drive<R: AsyncRead + Unpin>(&mut self, mut reader: R) -> Result<(), SessionError> {
        loop {
            // read_exact 不是取消安全的；完成通知可以穿插处理，但必须保留同一个半帧读取 future
            let incoming = read_frame(&mut reader);
            tokio::pin!(incoming);
            let frame = loop {
                tokio::select! {
                    biased;
                    writer = &mut self.writer => {
                        return writer.map_err(|_| SessionError::Closed)?;
                    }
                    Some(finished) = self.completions.recv() => {
                        self.finish_call(finished)?;
                    }
                    frame = &mut incoming => {
                        break frame.map_err(|error| match error {
                            FrameError::Io(io_error)
                                if io_error.kind() == std::io::ErrorKind::UnexpectedEof =>
                            {
                                SessionError::Closed
                            }
                            error => SessionError::Frame(error),
                        })?;
                    }
                }
            };
            if self.handle_frame(frame)? {
                return Ok(());
            }
        }
    }

    fn handle_frame(&mut self, frame: Frame) -> Result<bool, SessionError> {
        match frame.message {
            Message::Call {
                id,
                method,
                context,
                params,
            } => {
                self.start_call(id, method, context, params, frame.payload)?;
                Ok(false)
            }
            Message::Credit { id, bytes, frames } if frame.payload.is_empty() => {
                if id == 0
                    || id.is_multiple_of(2)
                    || id > self.last_call
                    || bytes == 0
                    || frames == 0
                {
                    return Err(SessionError::Protocol);
                }
                // End/Cancelled 已进入同一 FIFO 后，宿主先前为已消费分块排队的
                // Credit 仍可能稍晚抵达
                // 此时调用资源已经回收，忽略迟到信用即可；
                // 未来或回调 ID 仍按协议错误处理
                if let Some(active) = self.active.get(&id) {
                    active.credits.grant(bytes, frames)?;
                }
                Ok(false)
            }
            Message::Cancel { id } if frame.payload.is_empty() => {
                self.cancel_call(id)?;
                Ok(false)
            }
            Message::Result { id, result } => {
                self.callbacks.resolve(Frame {
                    message: Message::Result { id, result },
                    payload: frame.payload,
                })?;
                Ok(false)
            }
            Message::Error { id, error } => {
                self.callbacks.resolve(Frame {
                    message: Message::Error { id, error },
                    payload: frame.payload,
                })?;
                Ok(false)
            }
            Message::Quiesce if frame.payload.is_empty() => {
                if !self.quiescing {
                    self.quiescing = true;
                    self.lifecycle.quiesce();
                }
                Ok(false)
            }
            Message::Shutdown if frame.payload.is_empty() => Ok(true),
            _ => Err(SessionError::Protocol),
        }
    }

    fn start_call(
        &mut self,
        id: u64,
        method: String,
        context: CallContext,
        params: Value,
        payload: Vec<u8>,
    ) -> Result<(), SessionError> {
        if id == 0
            || id.is_multiple_of(2)
            || id <= self.last_call
            || context.call_id != id
            || context.instance_id != self.instance_id
            || context.generation != self.generation
            || context.incarnation != self.incarnation
            || context.timeout_ms == 0
            || Duration::from_millis(context.timeout_ms) > self.config.maximum_call_timeout
            || method.is_empty()
            || method.len() > 128
        {
            return Err(SessionError::Protocol);
        }
        self.last_call = id;
        if self.quiescing {
            return self.output.send_control(fault_frame(
                id,
                PluginFault::new(ErrorCode::Capacity, "plugin is quiescing"),
            ));
        }
        if self.active.len() >= self.config.maximum_calls {
            return self.output.send_control(fault_frame(
                id,
                PluginFault::new(ErrorCode::Capacity, "plugin call capacity is exhausted"),
            ));
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(context.timeout_ms))
            .ok_or(SessionError::Protocol)?;
        let deadline = Some(deadline);
        let (deadline_source, callback_deadline) = tokio::sync::watch::channel(deadline);
        let credits = Arc::new(CreditWindow::default());
        let (cancellation, cancellation_signal) = CancellationSource::new();
        let host = HostClient {
            parent_id: id,
            deadline: callback_deadline,
            cancellation: cancellation_signal.clone(),
            callbacks: Arc::clone(&self.callbacks),
            output: self.output.clone(),
            maximum_stream_chunk_bytes: self.config.maximum_stream_chunk_bytes,
            credits: credits.clone(),
        };
        let call = PluginCall {
            method,
            context: context.clone(),
            params,
            payload,
            host,
            cancellation: cancellation_signal,
        };
        let handler = Arc::clone(&self.handler);
        let output = self.output.clone();
        let completed = self.completed.clone();
        let task_credits = Arc::clone(&credits);
        let maximum_stream_chunk_bytes = self.config.maximum_stream_chunk_bytes;
        let maximum_chunks = self.config.maximum_buffered_stream_chunks;
        let task_context = context.clone();
        let lifecycle = self.lifecycle.clone();
        let task = tokio::spawn(async move {
            let guard = CompletionGuard::new(id, completed);
            let result = run_call(
                handler,
                call,
                task_context,
                task_credits,
                output,
                lifecycle,
                deadline,
                deadline_source,
                maximum_stream_chunk_bytes,
                maximum_chunks,
            )
            .await;
            guard.finish(if result.is_ok() {
                CallTaskOutcome::Complete
            } else {
                CallTaskOutcome::TransportFailed
            });
        });
        self.active.insert(
            id,
            ActiveCall {
                context,
                credits,
                cancellation,
                task,
            },
        );
        Ok(())
    }

    fn cancel_call(&mut self, id: u64) -> Result<(), SessionError> {
        if id == 0 || id.is_multiple_of(2) || id > self.last_call {
            return Err(SessionError::Protocol);
        }
        if let Some(active) = self.active.remove(&id) {
            active.cancellation.cancel();
            self.callbacks.finish_parent(id);
            self.lifecycle.cancel(active.context);
            active.task.abort();
        }
        self.output
            .send_control(Frame::control(Message::Cancelled { id }))
    }

    fn finish_call(&mut self, finished: CallFinished) -> Result<(), SessionError> {
        let Some(_active) = self.active.remove(&finished.id) else {
            return Ok(());
        };
        self.callbacks.finish_parent(finished.id);
        match finished.outcome {
            CallTaskOutcome::Complete => Ok(()),
            CallTaskOutcome::TransportFailed => Err(SessionError::Closed),
            CallTaskOutcome::HandlerStopped => Err(SessionError::HandlerStopped),
        }
    }

    async fn close(&mut self) {
        for (id, active) in std::mem::take(&mut self.active) {
            active.cancellation.cancel();
            self.callbacks.finish_parent(id);
            self.lifecycle.cancel(active.context);
            active.task.abort();
        }
        self.callbacks.close();
        self.lifecycle.shutdown();
        self.writer.abort();
        let _ = (&mut self.writer).await;
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "调用任务显式接收其完整资源边界，避免隐藏第二份会话状态"
)]
async fn run_call(
    handler: Arc<dyn PluginHandler>,
    call: PluginCall,
    context: CallContext,
    credits: Arc<CreditWindow>,
    output: Output,
    lifecycle: LifecycleHooks,
    deadline: Option<Instant>,
    deadline_source: tokio::sync::watch::Sender<Option<Instant>>,
    maximum_stream_chunk_bytes: usize,
    maximum_chunks: usize,
) -> Result<(), SessionError> {
    let id = context.call_id;
    let cancellation = call.cancellation.clone();
    let response = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Ok(()),
        () = wait_deadline(deadline) => {
            lifecycle.cancel(context.clone());
            output.send_control(fault_frame(
                id,
                PluginFault::new(ErrorCode::Timeout, "plugin call timed out"),
            ))?;
            return Ok(());
        }
        response = handler.call(call) => response,
    };
    let mut reply = match response {
        Ok(reply) => reply,
        Err(error) => {
            let _ = send_initial(
                &output,
                bounded_fault_frame(id, error),
                &context,
                &lifecycle,
                deadline,
                &cancellation,
            )
            .await?;
            return Ok(());
        }
    };
    let initial = Frame {
        message: Message::Result {
            id,
            result: reply.result,
        },
        payload: reply.payload,
    };
    if validate_frame(&initial).is_err() {
        output.send_control(fault_frame(
            id,
            PluginFault::new(
                ErrorCode::InvalidInput,
                "plugin response metadata exceeds its limit",
            ),
        ))?;
        return Ok(());
    }
    let Some(mut stream) = reply.stream.take() else {
        let _ = send_initial(
            &output,
            initial,
            &context,
            &lifecycle,
            deadline,
            &cancellation,
        )
        .await?;
        return Ok(());
    };
    let window_bytes = match credits.initial_window(deadline, &cancellation).await {
        Ok(window) => window,
        Err(SessionError::Timeout) => {
            lifecycle.cancel(context.clone());
            output.send_control(fault_frame(
                id,
                PluginFault::new(ErrorCode::Timeout, "stream credit timed out"),
            ))?;
            return Ok(());
        }
        Err(SessionError::Cancelled) => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Err(fault) =
        stream.validate_buffered(maximum_chunks, maximum_stream_chunk_bytes, window_bytes)
    {
        output.send_control(fault_frame(id, fault))?;
        return Ok(());
    }
    if !send_initial(
        &output,
        initial,
        &context,
        &lifecycle,
        deadline,
        &cancellation,
    )
    .await?
    {
        return Ok(());
    }
    let deadline = if context.resource_stream {
        None
    } else {
        deadline
    };
    deadline_source.send_replace(deadline);
    let mut sequence = 0_u64;
    loop {
        let item = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(()),
            () = wait_deadline(deadline) => {
                lifecycle.cancel(context.clone());
                output.send_control(end_frame(
                    id,
                    Some(PluginFault::new(ErrorCode::Timeout, "plugin stream timed out")),
                ))?;
                return Ok(());
            }
            item = stream.next() => item,
        };
        let Some(item) = item else {
            output.send_control(end_frame(id, None))?;
            return Ok(());
        };
        let payload = match item {
            Ok(payload) => payload,
            Err(fault) => {
                output.send_control(bounded_end_frame(id, fault))?;
                return Ok(());
            }
        };
        if let Err(fault) =
            validate_stream_chunk(&payload, maximum_stream_chunk_bytes, window_bytes)
        {
            output.send_control(end_frame(id, Some(fault)))?;
            return Ok(());
        }
        match credits
            .take(payload.len() as u64, deadline, &cancellation)
            .await
        {
            Ok(()) => {}
            Err(SessionError::Timeout) => {
                lifecycle.cancel(context.clone());
                output.send_control(end_frame(
                    id,
                    Some(PluginFault::new(
                        ErrorCode::Timeout,
                        "stream credit timed out",
                    )),
                ))?;
                return Ok(());
            }
            Err(SessionError::Cancelled) => return Ok(()),
            Err(error) => return Err(error),
        }
        let frame = Frame {
            message: Message::Stream { id, sequence },
            payload,
        };
        if !send_stream_data(
            &output,
            frame,
            id,
            &context,
            &lifecycle,
            deadline,
            &cancellation,
        )
        .await?
        {
            return Ok(());
        }
        sequence = sequence.checked_add(1).ok_or(SessionError::Protocol)?;
    }
}

async fn send_initial(
    output: &Output,
    frame: Frame,
    context: &CallContext,
    lifecycle: &LifecycleHooks,
    deadline: Option<Instant>,
    cancellation: &CallCancellation,
) -> Result<bool, SessionError> {
    match output.send_data_until(frame, deadline, cancellation).await {
        Ok(()) => Ok(true),
        Err(SessionError::Cancelled) => Ok(false),
        Err(SessionError::Timeout) => {
            lifecycle.cancel(context.clone());
            output.send_control(fault_frame(
                context.call_id,
                PluginFault::new(ErrorCode::Timeout, "plugin call timed out"),
            ))?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

async fn send_stream_data(
    output: &Output,
    frame: Frame,
    id: u64,
    context: &CallContext,
    lifecycle: &LifecycleHooks,
    deadline: Option<Instant>,
    cancellation: &CallCancellation,
) -> Result<bool, SessionError> {
    match output.send_data_until(frame, deadline, cancellation).await {
        Ok(()) => Ok(true),
        Err(SessionError::Cancelled) => Ok(false),
        Err(SessionError::Timeout) => {
            lifecycle.cancel(context.clone());
            output.send_control(end_frame(
                id,
                Some(PluginFault::new(
                    ErrorCode::Timeout,
                    "plugin stream timed out",
                )),
            ))?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[derive(Default)]
struct CreditWindow {
    state: Mutex<CreditState>,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct CreditState {
    bytes: u64,
    frames: u64,
    maximum_bytes: u64,
}

impl CreditWindow {
    fn grant(&self, bytes: u32, frames: u32) -> Result<(), SessionError> {
        if bytes == 0 || frames == 0 {
            return Err(SessionError::Protocol);
        }
        let mut state = lock_unpoisoned(&self.state);
        state.bytes = state
            .bytes
            .checked_add(u64::from(bytes))
            .ok_or(SessionError::Protocol)?;
        state.frames = state
            .frames
            .checked_add(u64::from(frames))
            .ok_or(SessionError::Protocol)?;
        state.maximum_bytes = state.maximum_bytes.max(state.bytes);
        drop(state);
        // 每个调用只有一个顺序消费 Credit 的任务；notify_one 会保留许可，避免
        // grant 恰好发生在状态检查和等待注册之间时丢失唤醒
        self.changed.notify_one();
        Ok(())
    }

    async fn initial_window(
        &self,
        deadline: Option<Instant>,
        cancellation: &CallCancellation,
    ) -> Result<u64, SessionError> {
        loop {
            let changed = self.changed.notified();
            {
                let state = lock_unpoisoned(&self.state);
                if state.bytes > 0 && state.frames > 0 {
                    return Ok(state.maximum_bytes);
                }
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(SessionError::Cancelled),
                () = wait_deadline(deadline) => return Err(SessionError::Timeout),
                () = changed => {}
            }
        }
    }

    async fn take(
        &self,
        bytes: u64,
        deadline: Option<Instant>,
        cancellation: &CallCancellation,
    ) -> Result<(), SessionError> {
        loop {
            let changed = self.changed.notified();
            {
                let mut state = lock_unpoisoned(&self.state);
                if bytes > state.maximum_bytes {
                    return Err(SessionError::Protocol);
                }
                if state.bytes >= bytes && state.frames > 0 {
                    state.bytes -= bytes;
                    state.frames -= 1;
                    return Ok(());
                }
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(SessionError::Cancelled),
                () = wait_deadline(deadline) => return Err(SessionError::Timeout),
                () = changed => {}
            }
        }
    }
}

fn validate_stream_chunk(
    payload: &[u8],
    maximum_stream_chunk_bytes: usize,
    window_bytes: u64,
) -> Result<(), PluginFault> {
    if payload.is_empty() {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "stream chunks must not be empty",
        ));
    }
    if payload.len() as u64 > window_bytes {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "response event exceeds the host stream credit window",
        ));
    }
    if payload.len() > maximum_stream_chunk_bytes {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "response event exceeds the stream chunk budget",
        ));
    }
    Ok(())
}

fn capacity_fault(message: &'static str) -> PluginFault {
    PluginFault::new(ErrorCode::Capacity, message)
}

fn fault_frame(id: u64, error: PluginFault) -> Frame {
    Frame::control(Message::Error { id, error })
}

fn end_frame(id: u64, error: Option<PluginFault>) -> Frame {
    Frame::control(Message::End { id, error })
}

fn bounded_fault_frame(id: u64, error: PluginFault) -> Frame {
    let frame = fault_frame(id, error);
    if validate_frame(&frame).is_ok() {
        frame
    } else {
        fault_frame(
            id,
            PluginFault::new(ErrorCode::Fault, "plugin error metadata exceeds its limit"),
        )
    }
}

fn bounded_end_frame(id: u64, error: PluginFault) -> Frame {
    let frame = end_frame(id, Some(error));
    if validate_frame(&frame).is_ok() {
        frame
    } else {
        end_frame(
            id,
            Some(PluginFault::new(
                ErrorCode::Fault,
                "plugin stream error metadata exceeds its limit",
            )),
        )
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn wait_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn callback_deadline(mut deadline: tokio::sync::watch::Receiver<Option<Instant>>) {
    loop {
        let current = *deadline.borrow_and_update();
        tokio::select! {
            () = wait_deadline(current) => return,
            changed = deadline.changed() => {
                if changed.is_err() { return wait_deadline(current).await; }
            }
        }
    }
}
