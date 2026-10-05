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
    client::{read_frame, validate_frame, write_frame},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore, mpsc, oneshot, watch};

use crate::{PreparedPackage, RpcStream, stream::StreamIngress};

use super::dispatch;

#[derive(Debug, Clone, Copy)]
pub struct RpcLimits {
    /// 受管回调与观察队列的缓冲预算，不是请求正文或 IPC 消息总量上限
    pub maximum_buffered_body_bytes: usize,
    pub maximum_calls: usize,
    pub maximum_callbacks: usize,
    pub handshake_timeout: Duration,
    pub maximum_call_timeout: Duration,
    pub maximum_stderr_bytes: usize,
}

impl Default for RpcLimits {
    fn default() -> Self {
        Self {
            maximum_buffered_body_bytes: 1024 * 1024,
            maximum_calls: 16,
            maximum_callbacks: 16,
            handshake_timeout: Duration::from_secs(5),
            maximum_call_timeout: Duration::from_secs(120),
            maximum_stderr_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    #[error("plugin process could not be started: {0}")]
    Start(#[source] ProcessStartError),
    #[error("plugin handshake does not match the prepared instance")]
    Handshake,
    #[error("plugin protocol is invalid")]
    Protocol,
    #[error("plugin returned an invalid response at {0:?}")]
    InvalidResponse(Stage),
    #[error("plugin process or transport has stopped")]
    Closed,
    #[error("plugin call exceeded its deadline")]
    Timeout,
    #[error("plugin session was cancelled")]
    Cancelled,
    #[error("plugin call capacity is exhausted")]
    Capacity,
    #[error("plugin call context is invalid")]
    Context,
    #[error("plugin returned an error")]
    Remote(PluginFault),
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
    fn begin(&self, _context: &CallContext) {}

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
    data: mpsc::Sender<Frame>,
    slots: Arc<Semaphore>,
    next_call: AsyncMutex<u64>,
    limits: RpcLimits,
    callbacks: Arc<dyn CallbackHandler>,
    _lifecycle: Option<RpcSessionLifecycle>,
}

impl RpcSession {
    /// 只用于已确认来自插件的合同错误，不接收业务错误或原始响应内容
    pub(crate) fn invalid_response(&self, stage: Stage) -> RpcError {
        let error = RpcError::InvalidResponse(stage);
        self.shared.fail(error.clone());
        error
    }

    pub(crate) fn decode_response<T: serde::de::DeserializeOwned>(
        &self,
        stage: Stage,
        value: serde_json::Value,
    ) -> Result<T, RpcError> {
        serde_json::from_value(value).map_err(|_| self.invalid_response(stage))
    }

    pub(crate) fn capability_version(
        &self,
        capability: gateway_plugin_sdk::Capability,
    ) -> Option<u32> {
        self.handshake
            .contributes
            .get(&capability)
            .map(|declaration| declaration.version)
    }
}

pub(crate) struct Shared {
    state: Mutex<State>,
    started_at: Instant,
    expected_stop: AtomicBool,
    failure_observer: Option<Arc<dyn Fn(Duration) + Send + Sync>>,
    pub control: mpsc::Sender<Frame>,
    pub process: ProcessControl,
    pub stopped: watch::Sender<bool>,
    callback_slots: Arc<Semaphore>,
}

struct State {
    failure: Option<RpcError>,
    calls: BTreeMap<u64, PendingCall>,
}

struct PendingCall {
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
        if let Some(stream) = self.stream.take() {
            stream.finish(result.as_ref().map(|_| ()).map_err(Clone::clone));
        }
        if let Some(sender) = self.result.take() {
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
        let call = state.calls.get_mut(&id).ok_or(RpcError::Protocol)?;
        if call.cancellation.is_some() {
            return Ok(());
        }
        if call.stream.is_some() && result.is_ok() {
            let initial = call.result.take().ok_or(RpcError::Protocol)?;
            call.handler.streaming(&call.context);
            let _ = initial.send(result);
        } else {
            let pending = state.calls.remove(&id).ok_or(RpcError::Protocol)?;
            drop(state);
            pending.finish(result);
        }
        Ok(())
    }

    pub fn stream_chunk(&self, id: u64, sequence: u64, payload: Vec<u8>) -> Result<(), RpcError> {
        let mut state = self.state();
        let call = state.calls.get_mut(&id).ok_or(RpcError::Protocol)?;
        if call.cancellation.is_some() {
            return Ok(());
        }
        if call.result.is_some() {
            return Err(RpcError::Protocol);
        }
        call.stream
            .as_mut()
            .ok_or(RpcError::Protocol)?
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
        let call = state.calls.remove(&id).ok_or(RpcError::Protocol)?;
        drop(state);
        if call.stream.is_none() || call.result.is_some() {
            call.finish(Err(RpcError::Protocol));
            return Err(RpcError::Protocol);
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
            .ok_or(RpcError::Protocol)?
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
            return Err(RpcError::Protocol);
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
        if self.control.try_send(frame).is_err() {
            self.fail(RpcError::Capacity);
        }
    }

    pub(super) fn try_callback_slot(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.callback_slots.clone().try_acquire_owned().ok()
    }

    async fn wait_for_callbacks(&self, maximum_callbacks: usize) {
        let Ok(maximum_callbacks) = u32::try_from(maximum_callbacks) else {
            return;
        };
        if let Ok(permits) = self
            .callback_slots
            .clone()
            .acquire_many_owned(maximum_callbacks)
            .await
        {
            drop(permits);
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
            || limits.maximum_callbacks == 0
            || limits.maximum_callbacks > 256
            || limits.maximum_buffered_body_bytes < 1024
            || limits.maximum_buffered_body_bytes > 16 * 1024 * 1024
            || limits.handshake_timeout.is_zero()
            || limits.maximum_call_timeout.is_zero()
        {
            return Err(RpcError::Context);
        }
        let manifest = package.package().manifest();
        let plugin_id = manifest.plugin_id().map_err(|_| RpcError::Handshake)?;
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
            return Err(RpcError::Handshake);
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
            .map_err(|_| RpcError::Handshake)?;
            let reply = read_frame(&mut output)
                .await
                .map_err(|_| RpcError::Handshake)?;
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
                _ => Err(RpcError::Handshake),
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
            mpsc::channel(limits.maximum_callbacks + limits.maximum_calls + 8);
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
            monitor.fail(RpcError::Closed);
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
            let (code, message) = match error {
                RpcError::Start(_) => ("process_start", "插件进程启动失败"),
                RpcError::Handshake => ("handshake", "插件握手失败"),
                RpcError::Protocol => ("protocol", "插件协议错误"),
                RpcError::InvalidResponse(stage) => (
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
                        Stage::Upstream => "上游适配阶段：插件返回了无效响应",
                        Stage::Observation => "观察阶段：插件返回了无效响应",
                        Stage::Management | Stage::PublicManagement => {
                            "管理阶段：插件返回了无效响应"
                        }
                        Stage::CommandLine => "命令行阶段：插件返回了无效响应",
                        Stage::Maintenance => "维护阶段：插件返回了无效响应",
                    },
                ),
                RpcError::Closed => ("closed", "插件进程或传输已停止"),
                RpcError::Timeout => ("timeout", "插件调用超时"),
                RpcError::Cancelled => ("cancelled", "插件会话已取消"),
                RpcError::Capacity => ("capacity", "插件调用容量已耗尽"),
                RpcError::Context => ("context", "插件调用上下文无效"),
                RpcError::Remote(_) => ("remote", "插件返回错误"),
            };
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

    fn call_deadline(
        &self,
        method: &str,
        context: &CallContext,
    ) -> Result<tokio::time::Instant, RpcError> {
        if context.instance_id != self.handshake.instance_id
            || context.generation != self.handshake.generation
            || context.incarnation != self.handshake.incarnation
            || context.timeout_ms == 0
            || Duration::from_millis(context.timeout_ms) > self.limits.maximum_call_timeout
            || method.is_empty()
            || method.len() > 128
        {
            return Err(RpcError::Context);
        }
        Ok(tokio::time::Instant::now() + Duration::from_millis(context.timeout_ms))
    }

    fn begin_call(
        &self,
        context: CallContext,
        deadline: tokio::time::Instant,
        stream: Option<StreamIngress>,
    ) -> Result<StartedCall, RpcError> {
        let id = context.call_id;
        let (result, received) = oneshot::channel();
        let mut state = self.shared.state();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.calls.len() >= self.limits.maximum_calls {
            return Err(RpcError::Capacity);
        }
        self.callbacks.begin(&context);
        state.calls.insert(
            id,
            PendingCall {
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
        let deadline = self.call_deadline(method, &context)?;
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
        validate_frame(&frame).map_err(|_| RpcError::Context)?;
        let started = self.begin_call(context, deadline, stream)?;
        let mut guard = CallGuard {
            shared: Arc::clone(&self.shared),
            id: started.id,
            armed: true,
        };
        let sent = tokio::time::timeout_at(started.deadline, async {
            self.data.send(frame).await.map_err(|_| RpcError::Closed)?;
            if let Some((bytes, frames)) = credit {
                // 初始信用跟在 Call 后走同一有序队列，不能被控制队列提前发送
                self.data
                    .send(Frame::control(Message::Credit {
                        id: started.id,
                        bytes,
                        frames,
                    }))
                    .await
                    .map_err(|_| RpcError::Closed)?;
            }
            Ok::<(), RpcError>(())
        })
        .await;
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
        let result = tokio::time::timeout_at(started.deadline, started.received).await;
        let reply = match result {
            Ok(Ok(result)) => {
                guard.armed = false;
                result?
            }
            Ok(Err(_)) => {
                self.shared.cancel(started.id, RpcError::Closed);
                guard.armed = false;
                return Err(RpcError::Closed);
            }
            Err(_) => {
                self.shared.cancel(started.id, RpcError::Timeout);
                guard.armed = false;
                return Err(RpcError::Timeout);
            }
        };
        Ok(CompletedCall {
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
        let _slot = self.slots.try_acquire().map_err(|_| RpcError::Capacity)?;
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
        let slot = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| RpcError::Capacity)?;
        let bytes = self.limits.maximum_buffered_body_bytes.min(256 * 1024) as u32;
        let frames = 32;
        let (stream, chunks, terminal) = StreamIngress::new(bytes, frames);
        let resource_stream = context.resource_stream;
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
            deadline: (!resource_stream).then_some(completed.deadline),
            id: completed.id,
            shared: Arc::clone(&self.shared),
            _slot: slot,
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
            self.shared.fail(RpcError::Closed);
            // 强制停止没有第二段宽限；退出和回调析构是返回前必须确认的事实
            self.settle_shutdown().await;
        }
    }

    async fn settle_shutdown(&self) {
        self.shared.process.exited().await;
        // 不等待监控任务调度；先撤销父调用并中止其回调，再确认回调 future 已析构
        self.shared.fail(RpcError::Closed);
        self.shared
            .wait_for_callbacks(self.limits.maximum_callbacks)
            .await;
    }
}

impl Drop for RpcSession {
    fn drop(&mut self) {
        self.shared.expect_stop();
        self.shared.fail(RpcError::Closed);
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
    deadline: tokio::time::Instant,
    received: oneshot::Receiver<Result<RpcReply, RpcError>>,
}

struct CompletedCall {
    id: u64,
    deadline: tokio::time::Instant,
    reply: RpcReply,
}
