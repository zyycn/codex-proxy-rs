//! 插件会话握手、在途调用分派与单一终结责任

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Semaphore, mpsc},
    task::JoinHandle,
    time::Instant,
};

use crate::{
    CallContext, ErrorCode, Frame, FrameError, Handshake, Message, PROTOCOL_VERSION, PluginFault,
};

use super::super::frame::{read_frame, validate_frame, write_frame};
use super::{
    CallCancellation, CallDeadline, HostClient, PluginCall, PluginHandler, SessionConfig,
    SessionError,
    credit::CreditWindow,
    lifecycle::{CancellationSource, LifecycleHooks},
    output::{Output, writer_loop},
    registry::CallbackRegistry,
    stream::validate_stream_chunk,
};

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
        let deadline = CallDeadline::new(deadline);
        let credits = Arc::new(CreditWindow::default());
        let (cancellation, cancellation_signal) = CancellationSource::new();
        let host = HostClient {
            parent_id: id,
            deadline: deadline.clone(),
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
    deadline: CallDeadline,
    maximum_stream_chunk_bytes: usize,
    maximum_chunks: usize,
) -> Result<(), SessionError> {
    let id = context.call_id;
    let cancellation = call.cancellation.clone();
    let response = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Ok(()),
        () = deadline.wait() => {
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
                &deadline,
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
            &deadline,
            &cancellation,
        )
        .await?;
        return Ok(());
    };
    let window_bytes = match credits.initial_window(&deadline, &cancellation).await {
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
        &deadline,
        &cancellation,
    )
    .await?
    {
        return Ok(());
    }
    if context.resource_stream {
        deadline.clear();
    }
    let mut sequence = 0_u64;
    loop {
        let item = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(()),
            () = deadline.wait() => {
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
            .take(payload.len() as u64, &deadline, &cancellation)
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
            &deadline,
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
    deadline: &CallDeadline,
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
    deadline: &CallDeadline,
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
