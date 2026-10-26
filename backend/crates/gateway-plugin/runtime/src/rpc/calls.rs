//! RPC 调用表唯一持有调度、回调、响应流和取消确认，统一完成调用资源回收

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gateway_host::process::ProcessControl;
use gateway_plugin_sdk::{
    CallContext, Frame, Message, PluginFault,
    client::{CallDeadline, CallDeadlinePause, EncodedFrame},
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

use super::{
    CallbackHandler, RpcError, RpcLimits, RpcReply, RpcSessionLoad, admission::ExecutionCapacity,
};
use crate::stream::StreamIngress;

pub(crate) struct Shared {
    state: Mutex<State>,
    started_at: Instant,
    pub(super) expected_stop: AtomicBool,
    failure_observer: Option<Arc<dyn Fn(Duration) + Send + Sync>>,
    pub control: mpsc::Sender<EncodedFrame>,
    pub process: ProcessControl,
    pub stopped: watch::Sender<bool>,
    callback_slots: Arc<Semaphore>,
    pub(super) execution: ExecutionCapacity,
    cancellations_changed: Notify,
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
    scheduling: Option<Scheduling>,
    callbacks: BTreeMap<u64, CallbackTask>,
}

// 调度状态只属于父任务；回调完成与恢复授权分别推进
enum Scheduling {
    Running {
        epoch: u64,
    },
    Suspended {
        epoch: u64,
        pause: Option<CallDeadlinePause>,
    },
    Resuming {
        epoch: u64,
        pause: Option<CallDeadlinePause>,
        task: tokio::task::AbortHandle,
    },
}

struct CallbackTask {
    task: tokio::task::AbortHandle,
    pauses_budget: bool,
    _permit: OwnedSemaphorePermit,
}

impl Drop for CallbackTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PendingCall {
    /// 关闭资源与封存副作用必须先于任何结果通知；取消确认只延续容量租约
    fn revoke(&mut self) {
        self.handler.finish(&self.context);
        self.callbacks.clear();
        self.stop_resumption();
    }

    fn stop_resumption(&mut self) {
        if let Some(Scheduling::Resuming { task, .. }) = self.scheduling.take() {
            task.abort();
        }
    }

    fn finish(mut self, result: Result<RpcReply, RpcError>) {
        tracing::debug!(
            event = "plugin.rpc.finished",
            instance_id = self.context.instance_id,
            call_id = self.context.call_id,
            stage = ?self.context.stage,
            outcome = result.as_ref().map_or_else(|error| error.diagnostic().0, |_| "success"),
            "插件 RPC 调用结束"
        );
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
        self.revoke();
    }
}

impl Shared {
    pub(super) fn new(
        limits: RpcLimits,
        process: ProcessControl,
        control: mpsc::Sender<EncodedFrame>,
        failure_observer: Option<Arc<dyn Fn(Duration) + Send + Sync>>,
    ) -> Self {
        let (stopped, _) = watch::channel(false);
        Self {
            state: Mutex::new(State {
                failure: None,
                calls: BTreeMap::new(),
            }),
            started_at: Instant::now(),
            expected_stop: AtomicBool::new(false),
            failure_observer,
            control,
            process,
            stopped,
            callback_slots: Arc::new(Semaphore::new(limits.maximum_callbacks)),
            execution: ExecutionCapacity::new(
                limits.maximum_executing_calls,
                limits.maximum_queued_calls,
                limits.maximum_queue_timeout,
            ),
            cancellations_changed: Notify::new(),
        }
    }

    pub(super) fn failure_diagnostic(&self) -> Option<(&'static str, &'static str)> {
        self.state().failure.as_ref().map(RpcError::diagnostic)
    }

    pub(super) fn load(&self, limits: RpcLimits, available_calls: usize) -> RpcSessionLoad {
        let state = self.state();
        let (available_execution, available_queue) = self.execution.available();
        RpcSessionLoad {
            active_calls: limits.maximum_calls - available_calls,
            executing_calls: limits.maximum_executing_calls - available_execution,
            queued_calls: limits.maximum_queued_calls - available_queue,
            resuming_calls: state
                .calls
                .values()
                .filter(|call| matches!(call.scheduling, Some(Scheduling::Resuming { .. })))
                .count(),
            pending_callbacks: limits.maximum_callbacks - self.callback_slots.available_permits(),
            active_streams: state
                .calls
                .values()
                .filter(|call| call.stream.is_some() && call.result.is_none())
                .count(),
        }
    }

    pub(super) fn begin_call(
        &self,
        context: CallContext,
        deadline: tokio::time::Instant,
        stream: Option<StreamIngress>,
        slot: Arc<OwnedSemaphorePermit>,
        execution: OwnedSemaphorePermit,
        handler: Arc<dyn CallbackHandler>,
    ) -> Result<StartedCall, RpcError> {
        let id = context.call_id;
        let (result, received) = oneshot::channel();
        let mut state = self.state();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        let deadline = CallDeadline::new(deadline);
        handler.begin(&context, &deadline);
        state.calls.insert(
            id,
            PendingCall {
                _slot: slot,
                execution: Some(execution),
                deadline: deadline.clone(),
                handler,
                transmitted: false,
                cancellation: None,
                context,
                result: Some(result),
                stream,
                scheduling: Some(Scheduling::Running { epoch: 0 }),
                callbacks: BTreeMap::new(),
            },
        );
        Ok(StartedCall {
            id,
            deadline,
            received,
        })
    }

    /// 替换维护任务前等待已取消的调用确认；未确认仍沿用会话的有界故障回收
    pub(crate) async fn settle_cancellations(&self) {
        let ids: Vec<_> = self
            .state()
            .calls
            .iter()
            .filter_map(|(id, call)| call.cancellation.is_some().then_some(*id))
            .collect();
        loop {
            let changed = self.cancellations_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !ids.iter().any(|id| self.is_cancelling(*id)) {
                break;
            }
            changed.await;
        }
        if self.failure_diagnostic().is_some() {
            self.process.exited().await;
        }
    }

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
        self.cancellations_changed.notify_waiters();
    }

    pub(super) fn expect_stop(&self) {
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
        if result.is_ok() && !matches!(call.scheduling, Some(Scheduling::Running { .. })) {
            return Err(RpcError::Protocol(None));
        }
        if call.stream.is_some() && result.is_ok() {
            if call.context.resource_stream {
                call.deadline.clear();
            }
            let initial = call.result.take().ok_or(RpcError::Protocol(None))?;
            call.execution.take();
            call.scheduling.take();
            call.handler.streaming(&call.context);
            drop(state);
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
            let call = state.calls.remove(&id);
            drop(state);
            if let Some(call) = call {
                call.finish(Err(error));
            }
            return;
        }
        call.cancellation = Some(error.clone());
        // 同步撤销不能跨越取消确认：收到确认的并发任务可能立即复用实例
        // CallbackHandler::finish 合同要求非阻塞；真正的等待与结果交付放在锁外
        call.revoke();
        let result = call.result.take();
        let stream = call.stream.take();
        tracing::debug!(
            event = "plugin.rpc.cancelled",
            instance_id = call.context.instance_id,
            call_id = id,
            stage = ?call.context.stage,
            reason = error.diagnostic().0,
            "插件 RPC 调用已撤销，等待进程确认"
        );
        drop(state);
        if let Some(stream) = stream {
            stream.finish(Err(error.clone()));
        }
        if let Some(result) = result {
            let _ = result.send(Err(error.clone()));
        }
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
        let call = state.calls.remove(&id);
        drop(state);
        drop(call);
        self.cancellations_changed.notify_waiters();
        Ok(())
    }

    pub fn track_callback(
        &self,
        parent_id: u64,
        id: u64,
        method: &str,
        task: tokio::task::AbortHandle,
        permit: OwnedSemaphorePermit,
    ) -> bool {
        let mut state = self.state();
        let Some(call) = state
            .calls
            .get_mut(&parent_id)
            .filter(|call| call.cancellation.is_none())
        else {
            task.abort();
            return false;
        };
        let pauses_budget = method == gateway_plugin_sdk::call::middleware::NEXT_METHOD;
        if !pauses_budget
            && let Some(Scheduling::Suspended { pause, .. } | Scheduling::Resuming { pause, .. }) =
                call.scheduling.as_mut()
        {
            pause.take();
        }
        call.callbacks.insert(
            id,
            CallbackTask {
                task,
                pauses_budget,
                _permit: permit,
            },
        );
        true
    }

    pub(super) fn consume_callback(&self, parent_id: u64, id: u64) -> Result<(), RpcError> {
        if id == 0 || !id.is_multiple_of(2) || parent_id == 0 || parent_id.is_multiple_of(2) {
            return Err(RpcError::Protocol(None));
        }
        // 取消确认可以与消费确认交错；确认只能撤销资源，不能重新创建调用
        if let Some(call) = self.state().calls.get_mut(&parent_id) {
            call.callbacks.remove(&id);
        }
        Ok(())
    }

    pub(super) fn suspend(&self, id: u64, epoch: u64) -> Result<(), RpcError> {
        if id == 0 || id.is_multiple_of(2) || epoch == 0 {
            return Err(RpcError::Protocol(None));
        }
        let mut state = self.state();
        let Some(call) = state.calls.get_mut(&id) else {
            return Ok(());
        };
        if call.cancellation.is_some() {
            return Ok(());
        }
        match call.scheduling.as_ref() {
            Some(Scheduling::Running { epoch: previous })
                if previous.checked_add(1) == Some(epoch) => {}
            _ => return Err(RpcError::Protocol(None)),
        }
        let pauses = !call.callbacks.is_empty()
            && call
                .callbacks
                .values()
                .all(|callback| callback.pauses_budget);
        let pause = pauses.then(|| call.deadline.pause()).flatten();
        call.execution.take();
        call.scheduling = Some(Scheduling::Suspended { epoch, pause });
        Ok(())
    }

    pub(super) fn request_resume(self: &Arc<Self>, id: u64, epoch: u64) -> Result<(), RpcError> {
        if id == 0 || id.is_multiple_of(2) || epoch == 0 {
            return Err(RpcError::Protocol(None));
        }
        let mut state = self.state();
        let Some(call) = state.calls.get_mut(&id) else {
            return Ok(());
        };
        if call.cancellation.is_some() {
            return Ok(());
        }
        if !matches!(call.scheduling, Some(Scheduling::Suspended { epoch: current, .. }) if current == epoch)
        {
            return Err(RpcError::Protocol(None));
        }
        let Some(Scheduling::Suspended { pause, .. }) = call.scheduling.take() else {
            return Err(RpcError::Protocol(None));
        };
        let shared = Arc::clone(self);
        let (ready, started) = oneshot::channel();
        let task = tokio::spawn(async move {
            if started.await.is_err() {
                return;
            }
            match shared.execution.resume().await {
                Ok(permit) => shared.grant_resume(id, epoch, permit),
                Err(error) => shared.cancel(id, error),
            }
        });
        call.scheduling = Some(Scheduling::Resuming {
            epoch,
            pause,
            task: task.abort_handle(),
        });
        let _ = ready.send(());
        Ok(())
    }

    fn grant_resume(&self, id: u64, epoch: u64, permit: OwnedSemaphorePermit) {
        let mut state = self.state();
        let Some(call) = state
            .calls
            .get_mut(&id)
            .filter(|call| call.cancellation.is_none())
        else {
            return;
        };
        if !matches!(call.scheduling, Some(Scheduling::Resuming { epoch: current, .. }) if current == epoch)
        {
            return;
        }
        call.execution = Some(permit);
        call.scheduling = Some(Scheduling::Running { epoch });
        drop(state);
        self.send_control(Frame::control(Message::Resumed { id, epoch }));
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

    pub(super) fn try_callback_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.callback_slots.clone().try_acquire_owned().ok()
    }

    pub(super) async fn wait_for_callbacks(&self, limits: RpcLimits) {
        if let Ok(permits) = self
            .callback_slots
            .clone()
            .acquire_many_owned(limits.maximum_callbacks as u32)
            .await
        {
            drop(permits);
        }
    }
}

pub(super) struct StartedCall {
    pub(super) id: u64,
    pub(super) deadline: CallDeadline,
    pub(super) received: oneshot::Receiver<Result<RpcReply, RpcError>>,
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
