//! 插件 RPC 帧的异步读写循环与入站消息分派

use std::sync::Arc;

use gateway_plugin_sdk::{
    ErrorCode, Frame, Message, PluginFault,
    client::{EncodedFrame, read_frame},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot},
};

use super::{CallbackHandler, RpcError, RpcReply, Shared};

pub(super) fn start<W, R>(
    writer: W,
    reader: R,
    data: mpsc::Receiver<EncodedFrame>,
    control: mpsc::Receiver<EncodedFrame>,
    shared: Arc<Shared>,
    callbacks: Arc<dyn CallbackHandler>,
) where
    W: AsyncWrite + Unpin + Send + 'static,
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(write_loop(writer, data, control, Arc::clone(&shared)));
    tokio::spawn(read_loop(reader, shared, callbacks));
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut data: mpsc::Receiver<EncodedFrame>,
    mut control: mpsc::Receiver<EncodedFrame>,
    shared: Arc<Shared>,
) {
    let mut stopped = shared.stopped.subscribe();
    loop {
        if *stopped.borrow() {
            return;
        }
        let frame = tokio::select! {
            biased;
            _ = stopped.changed() => return,
            frame = control.recv() => frame,
            frame = data.recv() => frame,
        };
        let Some(frame) = frame else {
            shared.fail(RpcError::Closed(None));
            return;
        };
        // 撤销尚未发送的调用时丢弃排队帧；已开始写入的 Call 必须先于 Cancel
        if let Message::Call { id, .. } = frame.message() {
            if !shared.mark_transmitted(*id) {
                continue;
            }
        } else if let Message::Credit { id, .. } = frame.message()
            && shared.context(*id).is_none()
        {
            continue;
        }
        let written = tokio::select! {
            biased;
            _ = stopped.changed() => return,
            result = frame.write(&mut writer) => result,
        };
        if let Err(source) = written {
            shared.fail(RpcError::Closed(Some(
                gateway_core::error::ErrorSource::new(source),
            )));
            return;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    shared: Arc<Shared>,
    callbacks: Arc<dyn CallbackHandler>,
) {
    let mut stopped = shared.stopped.subscribe();
    let mut last_callback = 0;
    loop {
        if *stopped.borrow() {
            return;
        }
        let frame = tokio::select! {
            biased;
            _ = stopped.changed() => return,
            frame = read_frame(&mut reader) => frame,
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(source) => {
                shared.fail(RpcError::Closed(Some(
                    gateway_core::error::ErrorSource::new(source),
                )));
                return;
            }
        };
        let result = match frame.message {
            Message::Suspend { id, epoch } if frame.payload.is_empty() => shared.suspend(id, epoch),
            Message::Resume { id, epoch } if frame.payload.is_empty() => {
                shared.request_resume(id, epoch)
            }
            Message::CallbackConsumed { id, parent_id } if frame.payload.is_empty() => {
                shared.consume_callback(parent_id, id)
            }
            Message::Cancelled { id } if frame.payload.is_empty() => {
                shared.acknowledge_cancellation(id)
            }
            Message::Stream { id, sequence } => shared.stream_chunk(id, sequence, frame.payload),
            Message::End { id, error } if frame.payload.is_empty() => shared.stream_end(id, error),
            Message::Result { id, result } => shared.finish(
                id,
                Ok(RpcReply {
                    result,
                    payload: frame.payload,
                }),
            ),
            Message::Error { id, error } if frame.payload.is_empty() => {
                shared.finish(id, Err(RpcError::Remote(error)))
            }
            Message::Callback {
                id,
                parent_id,
                method,
                params,
            } => {
                if id == 0 || id % 2 != 0 || id <= last_callback {
                    Err(RpcError::Protocol(None))
                } else if let Some(context) = shared.context(parent_id) {
                    last_callback = id;
                    if let Some(permit) = shared.try_callback_slot() {
                        let handler = Arc::clone(&callbacks);
                        let response = Arc::clone(&shared);
                        let (ready, started) = oneshot::channel();
                        let callback_method = method.clone();
                        let task = tokio::spawn(async move {
                            // 先把任务归属登记到父调用，再允许执行宿主操作
                            if started.await.is_err() {
                                return;
                            }
                            let result = handler.call(context, method, params, frame.payload).await;
                            let reply = match result {
                                Ok(reply) => Frame {
                                    message: Message::Result {
                                        id,
                                        result: reply.result,
                                    },
                                    payload: reply.payload,
                                },
                                Err(error) => Frame::control(Message::Error { id, error }),
                            };
                            // 宿主回调的局部编码错误只能结束该调用，不能关闭共享写通道
                            match EncodedFrame::new(reply) {
                                Ok(reply) => response.send_encoded_control(reply),
                                Err(_) => response.send_control(Frame::control(Message::Error {
                                    id,
                                    error: PluginFault::new(
                                        ErrorCode::Fault,
                                        "host callback response cannot be encoded",
                                    ),
                                })),
                            }
                        });
                        if shared.track_callback(
                            parent_id,
                            id,
                            &callback_method,
                            task.abort_handle(),
                            permit,
                        ) {
                            let _ = ready.send(());
                        }
                    } else {
                        shared.send_control(Frame::control(Message::Error {
                            id,
                            error: PluginFault::new(
                                ErrorCode::Capacity,
                                "callback capacity is exhausted",
                            ),
                        }));
                    }
                    Ok(())
                } else if shared.is_cancelling(parent_id) {
                    last_callback = id;
                    shared.send_control(Frame::control(Message::Error {
                        id,
                        error: PluginFault::new(ErrorCode::Cancelled, "parent call was cancelled"),
                    }));
                    Ok(())
                } else {
                    Err(RpcError::Protocol(None))
                }
            }
            _ => Err(RpcError::Protocol(None)),
        };
        if let Err(error) = result {
            shared.fail(error);
            return;
        }
    }
}
