//! 插件 RPC 会话的握手、对外调用、准入和进程关闭协调

use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use gateway_host::process::{ProcessSpec, ProcessStartError, ProcessSupervisor};
use gateway_plugin_sdk::{
    CallBudgetSource, CallContext, Frame, Handshake, Message, PROTOCOL_VERSION,
    ResolvedCallBudgets, Stage,
    client::{CallDeadline, EncodedFrame, read_frame, write_frame},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, mpsc};

use super::{
    CallbackHandler, RpcError, RpcLimits, RpcReply, RpcSessionDiagnostic, RpcSessionLifecycle,
    RpcSessionLoad, Shared, dispatch,
};
use crate::{PreparedPackage, RpcStream, stream::StreamIngress};

pub struct RpcSession {
    shared: Arc<Shared>,
    handshake: Handshake,
    budgets: ResolvedCallBudgets,
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

impl RpcSession {
    pub fn budget(&self, stage: Stage) -> Duration {
        self.budgets.budget(stage)
    }

    #[must_use]
    pub(crate) fn budget_source(&self, stage: Stage) -> CallBudgetSource {
        self.budgets.source(stage)
    }

    /// 替换维护任务前确认旧调用已取消或所属进程退出
    pub(crate) async fn settle_cancellations(&self) {
        self.shared.settle_cancellations().await;
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
            || limits.maximum_callbacks > 512
            || limits.maximum_buffered_body_bytes < 1024
            || limits.maximum_buffered_body_bytes > 16 * 1024 * 1024
            || limits.handshake_timeout.is_zero()
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
                    call_budgets,
                } if protocol_version == handshake.protocol_version
                    && incarnation == handshake.incarnation
                    && reply.payload.is_empty() =>
                {
                    ResolvedCallBudgets::resolve(&call_budgets).map_err(RpcError::Budget)
                }
                _ => Err(RpcError::Handshake(None)),
            }
        })
        .await
        .map_err(|_| RpcError::Timeout)
        .and_then(|result| result);
        let budgets = match ready {
            Ok(budgets) => budgets,
            Err(error) => {
                if let Some(lifecycle) = &lifecycle {
                    // 只有 Ready 之后的存活才可复位预算；长时间卡在握手不等于稳定运行
                    lifecycle.fail(Duration::ZERO);
                }
                process.stop();
                process.exited().await;
                return Err(error);
            }
        };
        let (data, data_receiver) = mpsc::channel(limits.maximum_calls);
        let (control, control_receiver) =
            mpsc::channel(limits.maximum_callbacks + 2 * limits.maximum_calls + 8);
        let shared = Arc::new(Shared::new(
            limits,
            process,
            control,
            lifecycle
                .as_ref()
                .map(|lifecycle| Arc::clone(&lifecycle.failure)),
        ));
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
            budgets,
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
        self.shared.failure_diagnostic().is_none()
    }

    pub(crate) fn load(&self) -> RpcSessionLoad {
        self.shared
            .load(self.limits, self.slots.available_permits())
    }

    /// 仅返回稳定分类与固定文案，绝不展开插件 fault、stderr 或请求内容
    pub(crate) fn diagnostic(&self) -> RpcSessionDiagnostic {
        if let Some((code, message)) = self.shared.failure_diagnostic() {
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
        // 调用方可以传入更短的外部剩余期限，不能扩大实例已协商的阶段预算
        let timeout = timeout.min(self.budget(stage));
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
            || Duration::from_millis(context.timeout_ms) > self.budget(context.stage)
            || method.is_empty()
            || method.len() > 128
        {
            return Err(RpcError::Context(None));
        }
        Ok(Duration::from_millis(context.timeout_ms))
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
        let queued_at = Instant::now();
        let execution = self.shared.execution.enter().await;
        tracing::debug!(
            event = "plugin.rpc.admission",
            instance_id = context.instance_id,
            stage = ?context.stage,
            request_id = context.request_id,
            attempt_id = context.attempt_id,
            wait_ms = queued_at.elapsed().as_millis() as u64,
            outcome = execution.as_ref().map_or_else(|error| error.diagnostic().0, |_| "admitted"),
            "插件 RPC 执行准入完成"
        );
        let execution = execution?;
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
        let started = self.shared.begin_call(
            context,
            deadline,
            stream,
            slot.clone(),
            execution,
            self.callbacks.clone(),
        )?;
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

struct CompletedCall {
    slot: Arc<OwnedSemaphorePermit>,
    id: u64,
    deadline: CallDeadline,
    reply: RpcReply,
}
