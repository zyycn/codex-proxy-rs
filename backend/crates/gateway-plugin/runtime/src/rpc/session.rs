//! 插件 RPC 会话的调用登记、超时取消、故障传播与关闭管理

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use futures::future::BoxFuture;
use gateway_host::process::{ProcessControl, ProcessSpec, ProcessStartError, ProcessSupervisor};
use gateway_plugin_sdk::{
    CallContext, Frame, Handshake, Message, PROTOCOL_VERSION, PluginFault, Stage,
    client::{CallDeadline, CallDeadlinePause, EncodedFrame, read_frame, write_frame},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

use crate::{PreparedPackage, RpcStream, stream::StreamIngress};

use super::{admission::ExecutionCapacity, dispatch};

#[derive(Debug, Clone, Copy)]
pub struct RpcLimits {
    /// 受管回调与观察队列的缓冲预算，不是请求正文或 IPC 消息总量上限
    pub maximum_buffered_body_bytes: usize,
    /// 包含初次排队、下游等待、取消确认和未消费流的总在途上限
    pub maximum_calls: usize,
    pub maximum_executing_calls: usize,
    pub maximum_queued_calls: usize,
    pub maximum_queue_timeout: Duration,
    pub maximum_callbacks: usize,
    pub handshake_timeout: Duration,
    pub maximum_call_timeout: Duration,
    pub maximum_stderr_bytes: usize,
}

impl Default for RpcLimits {
    fn default() -> Self {
        Self {
            maximum_buffered_body_bytes: 1024 * 1024,
            maximum_calls: 256,
            maximum_executing_calls: 64,
            maximum_queued_calls: 64,
            maximum_queue_timeout: Duration::from_secs(30),
            maximum_callbacks: 64,
            handshake_timeout: Duration::from_secs(5),
            maximum_call_timeout: Duration::from_secs(120),
            maximum_stderr_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcError {
    #[error("plugin process could not be started: {0}")]
    Start(#[source] ProcessStartError),
    #[error("plugin handshake does not match the prepared instance")]
    Handshake(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin protocol is invalid")]
    Protocol(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin returned an invalid response at {0:?}")]
    InvalidResponse(Stage, #[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin process or transport has stopped")]
    Closed(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin call exceeded its deadline")]
    Timeout,
    #[error("plugin session was cancelled")]
    Cancelled,
    #[error("plugin call capacity is exhausted")]
    Capacity,
    #[error("plugin admission capacity is exhausted")]
    Overloaded,
    #[error("plugin call context is invalid")]
    Context(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin returned an error")]
    Remote(PluginFault),
}

impl RpcError {
    pub(crate) fn is_overloaded(&self) -> bool {
        matches!(self, Self::Overloaded)
            || matches!(self, Self::Remote(fault) if fault.code == gateway_plugin_sdk::ErrorCode::Capacity)
    }

    /// 会话状态与请求错误共用安全分类，不展开插件返回正文
    pub(crate) fn diagnostic(&self) -> (&'static str, &'static str) {
        match self {
            RpcError::Start(_) => ("process_start", "插件进程启动失败"),
            RpcError::Handshake(_) => ("handshake", "插件握手失败"),
            RpcError::Protocol(_) => ("protocol", "插件协议错误"),
            RpcError::InvalidResponse(stage, _) => (
                "invalid_response",
                match stage {
                    Stage::Registration => "注册阶段：插件返回了无效响应",
                    Stage::Configuration => "配置阶段：插件返回了无效响应",
                    Stage::Authentication => "认证阶段：插件返回了无效响应",
                    Stage::Routing => "路由阶段：插件返回了无效响应",
                    Stage::Scheduling => "调度阶段：插件返回了无效响应",
                    Stage::Retry => "重试阶段：插件返回了无效响应",
                    Stage::Http => "HTTP 中间件：插件返回了无效响应",
                    Stage::Service => "服务中间件：插件返回了无效响应",
                    Stage::WebSocket => "WebSocket 中间件：插件返回了无效响应",
                    Stage::Request => "请求中间件：插件返回了无效响应",
                    Stage::Attempt => "尝试中间件：插件返回了无效响应",
                    Stage::Outbound => "出站准备中间件：插件返回了无效响应",
                    Stage::Upstream => "上游适配阶段：插件返回了无效响应",
                    Stage::Observation => "观察阶段：插件返回了无效响应",
                    Stage::Management | Stage::PublicManagement => "管理阶段：插件返回了无效响应",
                    Stage::CommandLine => "命令行阶段：插件返回了无效响应",
                    Stage::Maintenance => "维护阶段：插件返回了无效响应",
                },
            ),
            RpcError::Closed(_) => ("closed", "插件进程或传输已停止"),
            RpcError::Timeout => ("timeout", "插件调用超时"),
            RpcError::Cancelled => ("cancelled", "插件会话已取消"),
            RpcError::Capacity => ("capacity", "插件调用容量已耗尽"),
            RpcError::Overloaded => ("overloaded", "插件暂时繁忙"),
            RpcError::Context(_) => ("context", "插件调用上下文无效"),
            RpcError::Remote(_) => ("remote", "插件返回错误"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcSessionDiagnostic {
    Ready,
    Quiescing,
    Failed {
        code: &'static str,
        message: &'static str,
    },
}

pub(crate) struct RpcSessionLifecycle {
    failure: Arc<dyn Fn(Duration) + Send + Sync>,
    end: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl RpcSessionLifecycle {
    pub(crate) fn new(
        failure: impl Fn(Duration) + Send + Sync + 'static,
        end: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        Self {
            failure: Arc::new(failure),
            end: Some(Box::new(end)),
        }
    }

    fn fail(&self, uptime: Duration) {
        (self.failure)(uptime);
    }
}

impl Drop for RpcSessionLifecycle {
    fn drop(&mut self) {
        if let Some(end) = self.end.take() {
            end();
        }
    }
}

/// 不派生 Debug，防止原始请求、凭据或模型输出进入宿主诊断
pub struct RpcReply {
    pub result: serde_json::Value,
    pub payload: Vec<u8>,
}

/// 具体回调在领域适配器校验账号、出站与执行身份；上下文只取自宿主在途调用表
pub trait CallbackHandler: Send + Sync {
    /// 在 Call 可见之前建立资源范围；不得执行 I/O 或调用 RPC
    fn begin(&self, _context: &CallContext, _deadline: &CallDeadline) {}

    /// 调用结束或取消时立即撤销其句柄；实现必须幂等且不能阻塞
    fn finish(&self, _context: &CallContext) {}

    /// 首个流式结果已到达；资源仍绑定原调用并在终态统一回收
    fn streaming(&self, _context: &CallContext) {}

    fn call(
        &self,
        context: CallContext,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>>;
}

pub struct RpcSession {
    shared: Arc<Shared>,
    handshake: Handshake,
    data: mpsc::Sender<EncodedFrame>,
    slots: Arc<Semaphore>,
    next_call: AsyncMutex<u64>,
    limits: RpcLimits,
    callbacks: Arc<dyn CallbackHandler>,
    _lifecycle: Option<RpcSessionLifecycle>,
}

impl RpcSession {
    pub(crate) fn decode_response<T: serde::de::DeserializeOwned>(
        &self,
        stage: Stage,
        value: serde_json::Value,
    ) -> Result<T, RpcError> {
        serde_json::from_value(value).map_err(|source| {
            RpcError::InvalidResponse(stage, Some(gateway_core::error::ErrorSource::new(source)))
        })
    }
}

pub(crate) struct Shared {
    state: Mutex<State>,
    started_at: Instant,
    expected_stop: AtomicBool,
    failure_observer: Option<Arc<dyn Fn(Duration) + Send + Sync>>,
    pub control: mpsc::Sender<EncodedFrame>,
    pub process: ProcessControl,
    pub stopped: watch::Sender<bool>,
    callback_slots: Arc<Semaphore>,
    next_slots: Arc<Semaphore>,
    execution: ExecutionCapacity,
}

struct State {
    failure: Option<RpcError>,
    calls: BTreeMap<u64, PendingCall>,
}

struct PendingCall {
    _slot: Arc<OwnedSemaphorePermit>,
    execution: Option<OwnedSemaphorePermit>,
    deadline: CallDeadline,
    handler: Arc<dyn CallbackHandler>,
    transmitted: bool,
    cancellation: Option<RpcError>,
    context: CallContext,
    result: Option<oneshot::Sender<Result<RpcReply, RpcError>>>,
    stream: Option<StreamIngress>,
    callbacks: Vec<tokio::task::AbortHandle>,
}

impl PendingCall {
    fn finish(mut self, result: Result<RpcReply, RpcError>) {
        for callback in self.callbacks.drain(..) {
            callback.abort();
        }
        let stream = self.stream.take();
        let sender = self.result.take();
        // 先同步关闭调用资源，Core 才能在收到结果时看到全部发送副作用
        drop(self);
        if let Some(stream) = stream {
            stream.finish(result.as_ref().map(|_| ()).map_err(Clone::clone));
        }
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }
}

impl Drop for PendingCall {
    fn drop(&mut self) {
        self.handler.finish(&self.context);
    }
}

impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn fail(&self, error: RpcError) {
        let uptime = self.started_at.elapsed();
        let (calls, failure_observer) = {
            let mut state = self.state();
            if state.failure.is_some() {
                return;
            }
            state.failure = Some(error.clone());
            let observer = (!self.expected_stop.load(Ordering::Acquire))
                .then(|| self.failure_observer.clone())
                .flatten();
            (std::mem::take(&mut state.calls), observer)
        };
        self.execution.close();
        for call in calls.into_values() {
            call.finish(Err(error.clone()));
        }
        if let Some(observer) = failure_observer {
            observer(uptime);
        }
        self.stopped.send_replace(true);
        self.process.stop();
    }

    fn expect_stop(&self) {
        self.expected_stop.store(true, Ordering::Release);
    }

    pub fn finish(&self, id: u64, result: Result<RpcReply, RpcError>) -> Result<(), RpcError> {
        let mut state = self.state();
        let call = state.calls.get_mut(&id).ok_or(RpcError::Protocol(None))?;
        if call.cancellation.is_some() {
            return Ok(());
        }
        if matches!(&result, Err(RpcError::Remote(fault)) if fault.code == gateway_plugin_sdk::ErrorCode::Timeout)
        {
            log_timeout(&call.context, "plugin", "initial_reply", "remote_timeout");
        }
        if call.stream.is_some() && result.is_ok() {
            if call.context.resource_stream {
                call.deadline.clear();
            }
            let initial = call.result.take().ok_or(RpcError::Protocol(None))?;
            call.execution.take();
            call.handler.streaming(&call.context);
            let _ = initial.send(result);
        } else {
            let pending = state.calls.remove(&id).ok_or(RpcError::Protocol(None))?;
            drop(state);
            pending.finish(result);
        }
        Ok(())
    }

    pub fn stream_chunk(&self, id: u64, sequence: u64, payload: Vec<u8>) -> Result<(), RpcError> {
        let mut state = self.state();
        let call = state.calls.get_mut(&id).ok_or(RpcError::Protocol(None))?;
        if call.cancellation.is_some() {
            return Ok(());
        }
        if call.result.is_some() {
            return Err(RpcError::Protocol(None));
        }
        call.stream
            .as_mut()
            .ok_or(RpcError::Protocol(None))?
            .receive(sequence, payload)
    }

    pub fn stream_end(&self, id: u64, error: Option<PluginFault>) -> Result<(), RpcError> {
        let mut state = self.state();
        if state
            .calls
            .get(&id)
            .is_some_and(|call| call.cancellation.is_some())
        {
            return Ok(());
        }
        let call = state.calls.remove(&id).ok_or(RpcError::Protocol(None))?;
        drop(state);
        if call.stream.is_none() || call.result.is_some() {
            call.finish(Err(RpcError::Protocol(None)));
            return Err(RpcError::Protocol(None));
        }
        let result = error.map_or_else(
            || {
                Ok(RpcReply {
                    result: serde_json::Value::Null,
                    payload: Vec::new(),
                })
            },
            |error| Err(RpcError::Remote(error)),
        );
        call.finish(result);
        Ok(())
    }

    pub fn release_stream_credit(&self, id: u64, bytes: u32) -> Result<(), RpcError> {
        let mut state = self.state();
        // 完成后仍可消费已排队数据，无须再给已关闭的流发信用
        let Some(call) = state.calls.get_mut(&id) else {
            return Ok(());
        };
        call.stream
            .as_mut()
            .ok_or(RpcError::Protocol(None))?
            .release(bytes)?;
        drop(state);
        self.send_control(Frame::control(Message::Credit {
            id,
            bytes,
            frames: 1,
        }));
        Ok(())
    }

    pub fn context(&self, id: u64) -> Option<CallContext> {
        self.state()
            .calls
            .get(&id)
            .filter(|call| call.cancellation.is_none())
            .map(|call| call.context.clone())
    }

    pub fn pause_next(&self, id: u64) -> Option<CallDeadlinePause> {
        let mut state = self.state();
        let call = state
            .calls
            .get_mut(&id)
            .filter(|call| call.cancellation.is_none())?;
        let pause = call.deadline.pause()?;
        call.execution.take();
        Some(pause)
    }

    pub async fn resume_next(&self, id: u64) -> Result<(), RpcError> {
        {
            let state = self.state();
            let call = state
                .calls
                .get(&id)
                .filter(|call| call.cancellation.is_none())
                .ok_or(RpcError::Cancelled)?;
            // 已交付响应头的流只占在途资源，不重新占用首结果执行名额
            if call.result.is_none() {
                return Ok(());
            }
        }
        let permit = self.execution.resume().await?;
        let mut state = self.state();
        let call = state
            .calls
            .get_mut(&id)
            .filter(|call| call.cancellation.is_none())
            .ok_or(RpcError::Cancelled)?;
        if call.result.is_some() {
            call.execution = Some(permit);
        }
        Ok(())
    }

    pub fn is_cancelling(&self, id: u64) -> bool {
        self.state()
            .calls
            .get(&id)
            .is_some_and(|call| call.cancellation.is_some())
    }

    pub fn mark_transmitted(&self, id: u64) -> bool {
        let mut state = self.state();
        let Some(call) = state.calls.get_mut(&id) else {
            return false;
        };
        if call.cancellation.is_some() {
            return false;
        }
        call.transmitted = true;
        true
    }

    /// 先撤销调用权限，再等待对端确认；只有无响应对端才终止整个 incarnation
    pub fn cancel(self: &Arc<Self>, id: u64, error: RpcError) {
        let mut state = self.state();
        let Some(call) = state.calls.get_mut(&id) else {
            return;
        };
        if call.cancellation.is_some() {
            return;
        }
        if matches!(error, RpcError::Timeout) {
            log_timeout(
                &call.context,
                "runtime",
                if call.result.is_some() {
                    "initial_reply"
                } else {
                    "response_stream"
                },
                "processing_budget_exhausted",
            );
        }
        if !call.transmitted {
            if let Some(call) = state.calls.remove(&id) {
                call.finish(Err(error));
            }
            return;
        }
        call.cancellation = Some(error.clone());
        call.handler.finish(&call.context);
        for callback in call.callbacks.drain(..) {
            callback.abort();
        }
        if let Some(result) = call.result.take() {
            let _ = result.send(Err(error.clone()));
        }
        if let Some(stream) = call.stream.take() {
            stream.finish(Err(error.clone()));
        }
        drop(state);
        self.send_control(Frame::control(Message::Cancel { id }));
        let shared = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if shared.is_cancelling(id) {
                shared.fail(error);
            }
        });
    }

    pub fn acknowledge_cancellation(&self, id: u64) -> Result<(), RpcError> {
        let mut state = self.state();
        if !state
            .calls
            .get(&id)
            .is_some_and(|call| call.cancellation.is_some())
        {
            return Err(RpcError::Protocol(None));
        }
        state.calls.remove(&id);
        Ok(())
    }

    pub fn track_callback(&self, parent_id: u64, callback: tokio::task::AbortHandle) -> bool {
        if let Some(call) = self
            .state()
            .calls
            .get_mut(&parent_id)
            .filter(|call| call.cancellation.is_none())
        {
            // 已完成回调的句柄不继续占用请求预算
            call.callbacks.retain(|handle| !handle.is_finished());
            call.callbacks.push(callback);
            true
        } else {
            callback.abort();
            false
        }
    }

    pub fn send_control(&self, frame: Frame) {
        let Ok(frame) = EncodedFrame::new(frame) else {
            self.fail(RpcError::Protocol(None));
            return;
        };
        self.send_encoded_control(frame);
    }

    pub(super) fn send_encoded_control(&self, frame: EncodedFrame) {
        if self.control.try_send(frame).is_err() {
            self.fail(RpcError::Capacity);
        }
    }

    pub(super) fn try_callback_slot(&self, method: &str) -> Option<OwnedSemaphorePermit> {
        let slots = if method == gateway_plugin_sdk::call::middleware::NEXT_METHOD {
            &self.next_slots
        } else {
            &self.callback_slots
        };
        slots.clone().try_acquire_owned().ok()
    }

    async fn wait_for_callbacks(&self, limits: RpcLimits) {
        for (slots, maximum) in [
            (&self.callback_slots, limits.maximum_callbacks),
            (&self.next_slots, limits.maximum_calls),
        ] {
            if let Ok(permits) = slots.clone().acquire_many_owned(maximum as u32).await {
                drop(permits);
            }
        }
    }
}

impl RpcSession {
    pub(crate) const fn maximum_call_timeout(&self) -> Duration {
        self.limits.maximum_call_timeout
    }

    pub async fn start(
        package: Arc<PreparedPackage>,
        handshake: Handshake,
        limits: RpcLimits,
        processes: &ProcessSupervisor,
        callbacks: Arc<dyn CallbackHandler>,
    ) -> Result<Self, RpcError> {
        Self::start_inner(package, handshake, limits, processes, callbacks, None).await
    }

    pub(crate) async fn start_supervised(
        package: Arc<PreparedPackage>,
        handshake: Handshake,
        limits: RpcLimits,
        processes: &ProcessSupervisor,
        callbacks: Arc<dyn CallbackHandler>,
        lifecycle: RpcSessionLifecycle,
    ) -> Result<Self, RpcError> {
        Self::start_inner(
            package,
            handshake,
            limits,
            processes,
            callbacks,
            Some(lifecycle),
        )
        .await
    }

    async fn start_inner(
        package: Arc<PreparedPackage>,
        handshake: Handshake,
        limits: RpcLimits,
        processes: &ProcessSupervisor,
        callbacks: Arc<dyn CallbackHandler>,
        lifecycle: Option<RpcSessionLifecycle>,
    ) -> Result<Self, RpcError> {
        if limits.maximum_calls == 0
            || limits.maximum_calls > 256
            || limits.maximum_executing_calls == 0
            || limits.maximum_executing_calls > 256
            || limits.maximum_queued_calls > 256
            || limits.maximum_queue_timeout.is_zero()
            || limits.maximum_callbacks == 0
            || limits.maximum_callbacks > 256
            || limits.maximum_buffered_body_bytes < 1024
            || limits.maximum_buffered_body_bytes > 16 * 1024 * 1024
            || limits.handshake_timeout.is_zero()
            || limits.maximum_call_timeout.is_zero()
        {
            return Err(RpcError::Context(None));
        }
        let manifest = package.package().manifest();
        let plugin_id = manifest.plugin_id().map_err(|source| {
            RpcError::Handshake(Some(gateway_core::error::ErrorSource::new(source)))
        })?;
        if handshake.protocol_version != PROTOCOL_VERSION
            || manifest
                .package
                .as_ref()
                .map(|package| package.protocol_version)
                != Some(handshake.protocol_version)
            || handshake.plugin_id != plugin_id
            || handshake.artifact_sha256 != package.package().digest()
            || handshake.contributes != manifest.contributes
            || handshake.instance_id.is_empty()
            || handshake.incarnation.is_empty()
            || handshake.generation == 0
        {
            return Err(RpcError::Handshake(None));
        }
        let deadline = tokio::time::Instant::now() + limits.handshake_timeout;
        let process = loop {
            match processes.spawn(ProcessSpec {
                executable: package.executable().to_owned(),
                directory: package.directory().to_owned(),
                maximum_stderr_bytes: limits.maximum_stderr_bytes,
            }) {
                Ok(process) => break process,
                Err(
                    error @ ProcessStartError::Spawn {
                        kind: std::io::ErrorKind::ExecutableFileBusy,
                        ..
                    },
                ) => {
                    // Unix 并发 fork 可能短暂继承解包时的写句柄（rust-lang/rust#114554）
                    // 仅重试尚未执行的文件忙错误，并与握手共享启动期限
                    let retry_at = tokio::time::Instant::now() + Duration::from_millis(10);
                    if retry_at >= deadline {
                        return Err(RpcError::Start(error));
                    }
                    tokio::time::sleep_until(retry_at).await;
                }
                Err(error) => return Err(RpcError::Start(error)),
            }
        };
        let gateway_host::process::ProcessConnection {
            mut input,
            mut output,
            control: process,
        } = process;
        let ready = tokio::time::timeout_at(deadline, async {
            write_frame(
                &mut input,
                &Frame::control(Message::Hello {
                    handshake: handshake.clone(),
                }),
            )
            .await
            .map_err(|source| {
                RpcError::Handshake(Some(gateway_core::error::ErrorSource::new(source)))
            })?;
            let reply = read_frame(&mut output).await.map_err(|source| {
                RpcError::Handshake(Some(gateway_core::error::ErrorSource::new(source)))
            })?;
            match reply.message {
                Message::Ready {
                    protocol_version,
                    incarnation,
                } if protocol_version == handshake.protocol_version
                    && incarnation == handshake.incarnation
                    && reply.payload.is_empty() =>
                {
                    Ok(())
                }
                _ => Err(RpcError::Handshake(None)),
            }
        })
        .await
        .map_err(|_| RpcError::Timeout)
        .and_then(|result| result);
        if let Err(error) = ready {
            if let Some(lifecycle) = &lifecycle {
                // 只有 Ready 之后的存活才可复位预算；长时间卡在握手不等于稳定运行
                lifecycle.fail(Duration::ZERO);
            }
            process.stop();
            process.exited().await;
            return Err(error);
        }
        let (data, data_receiver) = mpsc::channel(limits.maximum_calls);
        let (control, control_receiver) =
            mpsc::channel(limits.maximum_callbacks + 2 * limits.maximum_calls + 8);
        let (stopped, _) = watch::channel(false);
        let callback_slots = Arc::new(Semaphore::new(limits.maximum_callbacks));
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                failure: None,
                calls: BTreeMap::new(),
            }),
            started_at: Instant::now(),
            expected_stop: AtomicBool::new(false),
            failure_observer: lifecycle
                .as_ref()
                .map(|lifecycle| Arc::clone(&lifecycle.failure)),
            control,
            process,
            stopped,
            callback_slots,
            next_slots: Arc::new(Semaphore::new(limits.maximum_calls)),
            execution: ExecutionCapacity::new(
                limits.maximum_executing_calls,
                limits.maximum_queued_calls,
                limits.maximum_queue_timeout,
            ),
        });
        dispatch::start(
            input,
            output,
            data_receiver,
            control_receiver,
            Arc::clone(&shared),
            callbacks.clone(),
        );
        let monitor = Arc::clone(&shared);
        let instance_id = handshake.instance_id.clone();
        tokio::spawn(async move {
            let reason = monitor.process.exited().await;
            if !monitor.expected_stop.load(Ordering::Acquire) {
                tracing::warn!(instance_id, exit = ?reason, "插件进程意外停止");
            }
            monitor.fail(RpcError::Closed(None));
            // 等进程退出后再回收其文件，避免 Windows 上删除正在执行的制品
            drop(package);
        });
        Ok(Self {
            shared,
            handshake,
            data,
            slots: Arc::new(Semaphore::new(limits.maximum_calls)),
            next_call: AsyncMutex::new(1),
            limits,
            callbacks,
            _lifecycle: lifecycle,
        })
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .is_none()
    }

    /// 仅返回稳定分类与固定文案，绝不展开插件 fault、stderr 或请求内容
    pub(crate) fn diagnostic(&self) -> RpcSessionDiagnostic {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = &state.failure {
            let (code, message) = error.diagnostic();
            return RpcSessionDiagnostic::Failed { code, message };
        }
        if self.slots.is_closed() {
            RpcSessionDiagnostic::Quiescing
        } else {
            RpcSessionDiagnostic::Ready
        }
    }

    #[must_use]
    pub fn context(&self, stage: gateway_plugin_sdk::Stage, timeout: Duration) -> CallContext {
        // 宿主生成的上下文受本会话预算约束；固定注册期限不能使较小的总预算无法启动
        let timeout = timeout.min(self.limits.maximum_call_timeout);
        CallContext {
            request: None,
            call_id: 0,
            instance_id: self.handshake.instance_id.clone(),
            generation: self.handshake.generation,
            incarnation: self.handshake.incarnation.clone(),
            stage,
            timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            resource_stream: false,
            resource_scope_id: uuid::Uuid::new_v4().to_string(),
            request_id: None,
            attempt_id: None,
            account_id: None,
            credential_revision: None,
        }
    }

    fn call_timeout(&self, method: &str, context: &CallContext) -> Result<Duration, RpcError> {
        if context.instance_id != self.handshake.instance_id
            || context.generation != self.handshake.generation
            || context.incarnation != self.handshake.incarnation
            || context.timeout_ms == 0
            || Duration::from_millis(context.timeout_ms) > self.limits.maximum_call_timeout
            || method.is_empty()
            || method.len() > 128
        {
            return Err(RpcError::Context(None));
        }
        Ok(Duration::from_millis(context.timeout_ms))
    }

    fn begin_call(
        &self,
        context: CallContext,
        deadline: tokio::time::Instant,
        stream: Option<StreamIngress>,
        slot: Arc<OwnedSemaphorePermit>,
        execution: OwnedSemaphorePermit,
    ) -> Result<StartedCall, RpcError> {
        let id = context.call_id;
        let (result, received) = oneshot::channel();
        let mut state = self.shared.state();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        let deadline = CallDeadline::new(deadline);
        self.callbacks.begin(&context, &deadline);
        state.calls.insert(
            id,
            PendingCall {
                _slot: slot,
                execution: Some(execution),
                deadline: deadline.clone(),
                handler: self.callbacks.clone(),
                transmitted: false,
                cancellation: None,
                context,
                result: Some(result),
                stream,
                callbacks: Vec::new(),
            },
        );
        Ok(StartedCall {
            id,
            deadline,
            received,
        })
    }

    async fn send_call(
        &self,
        method: &str,
        context: CallContext,
        params: serde_json::Value,
        payload: Vec<u8>,
        stream: Option<StreamIngress>,
        credit: Option<(u32, u32)>,
    ) -> Result<CompletedCall, RpcError> {
        let timeout = self.call_timeout(method, &context)?;
        let slot = Arc::new(self.slots.clone().try_acquire_owned().map_err(|error| {
            if matches!(error, tokio::sync::TryAcquireError::Closed) {
                RpcError::Closed(None)
            } else {
                RpcError::Overloaded
            }
        })?);
        let execution = self.shared.execution.enter().await?;
        let deadline = tokio::time::Instant::now() + timeout;
        // FIFO 锁同时拥有 ID 与 data 入队顺序；Call/初始 Credit 入队后必须立即释放，
        // 不能把插件回复或重入回调纳入串行区，也不能为等待锁重置调用期限
        let mut next_call = tokio::time::timeout_at(deadline, self.next_call.lock())
            .await
            .map_err(|_| RpcError::Timeout)?;
        let id = *next_call;
        *next_call = id.checked_add(2).ok_or(RpcError::Capacity)?;
        let mut context = context;
        context.call_id = id;
        let frame = Frame {
            message: Message::Call {
                id,
                method: method.to_owned(),
                context: context.clone(),
                params,
            },
            payload,
        };
        // 本地元数据错误不能进入写队列并关闭整个会话
        let frame = EncodedFrame::new(frame).map_err(|source| {
            RpcError::Context(Some(gateway_core::error::ErrorSource::new(source)))
        })?;
        let started = self.begin_call(context, deadline, stream, slot.clone(), execution)?;
        let mut guard = CallGuard {
            shared: Arc::clone(&self.shared),
            id: started.id,
            armed: true,
        };
        let send = async {
            self.data
                .send(frame)
                .await
                .map_err(|_| RpcError::Closed(None))?;
            if let Some((bytes, frames)) = credit {
                // 初始信用跟在 Call 后走同一有序队列，不能被控制队列提前发送
                self.data
                    .send(
                        EncodedFrame::new(Frame::control(Message::Credit {
                            id: started.id,
                            bytes,
                            frames,
                        }))
                        .map_err(|source| {
                            RpcError::Context(Some(gateway_core::error::ErrorSource::new(source)))
                        })?,
                    )
                    .await
                    .map_err(|_| RpcError::Closed(None))?;
            }
            Ok::<(), RpcError>(())
        };
        let sent = tokio::select! {
            biased;
            () = started.deadline.wait() => Err(()),
            result = send => Ok(result),
        };
        drop(next_call);
        match sent {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.shared.cancel(started.id, error.clone());
                guard.armed = false;
                return Err(error);
            }
            Err(_) => {
                self.shared.cancel(started.id, RpcError::Timeout);
                guard.armed = false;
                return Err(RpcError::Timeout);
            }
        }
        let result = tokio::select! {
            biased;
            () = started.deadline.wait() => Err(()),
            result = started.received => Ok(result),
        };
        let reply = match result {
            Ok(Ok(result)) => {
                guard.armed = false;
                result?
            }
            Ok(Err(_)) => {
                self.shared.cancel(started.id, RpcError::Closed(None));
                guard.armed = false;
                return Err(RpcError::Closed(None));
            }
            Err(_) => {
                self.shared.cancel(started.id, RpcError::Timeout);
                guard.armed = false;
                return Err(RpcError::Timeout);
            }
        };
        Ok(CompletedCall {
            slot,
            id: started.id,
            deadline: started.deadline,
            reply,
        })
    }

    pub async fn call(
        &self,
        method: &str,
        context: CallContext,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> Result<RpcReply, RpcError> {
        self.send_call(method, context, params, payload, None, None)
            .await
            .map(|completed| completed.reply)
    }

    pub async fn call_stream(
        self: &Arc<Self>,
        method: &str,
        context: CallContext,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> Result<RpcStream, RpcError> {
        let bytes = self.limits.maximum_buffered_body_bytes.min(256 * 1024) as u32;
        let frames = 32;
        let (stream, chunks, terminal) = StreamIngress::new(bytes, frames);
        let completed = self
            .send_call(
                method,
                context,
                params,
                payload,
                Some(stream),
                Some((bytes, frames)),
            )
            .await?;
        Ok(RpcStream {
            initial: completed.reply,
            chunks,
            terminal: Some(terminal),
            deadline: completed.deadline,
            id: completed.id,
            shared: Arc::clone(&self.shared),
            _slot: completed.slot,
            _session: Arc::clone(self),
        })
    }

    pub fn quiesce(&self) {
        self.shared.expect_stop();
        self.slots.close();
        self.shared.send_control(Frame::control(Message::Quiesce));
    }

    pub async fn shutdown(&self, grace: Duration) {
        let deadline = tokio::time::Instant::now() + grace;
        self.shutdown_until(deadline).await;
    }

    pub(crate) async fn shutdown_until(&self, deadline: tokio::time::Instant) {
        self.shared.expect_stop();
        self.shared.send_control(Frame::control(Message::Shutdown));
        if tokio::time::timeout_at(deadline, self.settle_shutdown())
            .await
            .is_err()
        {
            self.shared.fail(RpcError::Closed(None));
            // 强制停止没有第二段宽限；退出和回调析构是返回前必须确认的事实
            self.settle_shutdown().await;
        }
    }

    async fn settle_shutdown(&self) {
        self.shared.process.exited().await;
        // 不等待监控任务调度；先撤销父调用并中止其回调，再确认回调 future 已析构
        self.shared.fail(RpcError::Closed(None));
        self.shared.wait_for_callbacks(self.limits).await;
    }
}

impl Drop for RpcSession {
    fn drop(&mut self) {
        self.shared.expect_stop();
        self.shared.fail(RpcError::Closed(None));
    }
}

struct CallGuard {
    shared: Arc<Shared>,
    id: u64,
    armed: bool,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.cancel(self.id, RpcError::Cancelled);
        }
    }
}

struct StartedCall {
    id: u64,
    deadline: CallDeadline,
    received: oneshot::Receiver<Result<RpcReply, RpcError>>,
}

struct CompletedCall {
    slot: Arc<OwnedSemaphorePermit>,
    id: u64,
    deadline: CallDeadline,
    reply: RpcReply,
}

fn log_timeout(
    context: &CallContext,
    owner: &'static str,
    phase: &'static str,
    reason: &'static str,
) {
    tracing::warn!(
        event = "plugin.rpc.timeout",
        owner,
        phase,
        instance_id = context.instance_id,
        call_id = context.call_id,
        request_id = context.request_id,
        attempt_id = context.attempt_id,
        stage = ?context.stage,
        budget_ms = context.timeout_ms,
        reason,
        "插件 RPC 调用超时"
    );
}
