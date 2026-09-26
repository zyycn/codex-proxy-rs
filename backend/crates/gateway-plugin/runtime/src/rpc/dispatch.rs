use std::sync::Arc;

use gateway_plugin_sdk::{
    ErrorCode, Frame, Message, Permission, PluginFault, Stage,
    client::{read_frame, validate_frame, write_frame},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot},
};

use super::session::{CallbackHandler, RpcError, RpcReply, Shared};

pub(super) fn start<W, R>(
    writer: W,
    reader: R,
    data: mpsc::Receiver<Frame>,
    control: mpsc::Receiver<Frame>,
    shared: Arc<Shared>,
    callbacks: Arc<dyn CallbackHandler>,
    permissions: Vec<Permission>,
) where
    W: AsyncWrite + Unpin + Send + 'static,
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(write_loop(writer, data, control, Arc::clone(&shared)));
    tokio::spawn(read_loop(reader, shared, callbacks, permissions));
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut data: mpsc::Receiver<Frame>,
    mut control: mpsc::Receiver<Frame>,
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
            shared.fail(RpcError::Closed);
            return;
        };
        // 撤销尚未发送的调用时丢弃排队帧；已开始写入的 Call 必须先于 Cancel。
        if let Message::Call { id, .. } = &frame.message {
            if !shared.mark_transmitted(*id) {
                continue;
            }
        } else if let Message::Credit { id, .. } = &frame.message
            && shared.context(*id).is_none()
        {
            continue;
        }
        let written = tokio::select! {
            biased;
            _ = stopped.changed() => return,
            result = write_frame(&mut writer, &frame) => result,
        };
        if written.is_err() {
            shared.fail(RpcError::Closed);
            return;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    shared: Arc<Shared>,
    callbacks: Arc<dyn CallbackHandler>,
    permissions: Vec<Permission>,
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
        let Ok(frame) = frame else {
            shared.fail(RpcError::Closed);
            return;
        };
        let result = match frame.message {
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
                    Err(RpcError::Protocol)
                } else if let Some(context) = shared.context(parent_id) {
                    last_callback = id;
                    if !callback_allowed(&method, context.stage, &permissions) {
                        shared.send_control(Frame::control(Message::Error {
                            id,
                            error: PluginFault::new(
                                ErrorCode::PermissionDenied,
                                "callback is not authorized in this stage",
                            ),
                        }));
                    } else if let Some(permit) = shared.try_callback_slot() {
                        let handler = Arc::clone(&callbacks);
                        let response = Arc::clone(&shared);
                        let (ready, started) = oneshot::channel();
                        let task = tokio::spawn(async move {
                            // 先把任务归属登记到父调用，再允许执行宿主操作。
                            if started.await.is_err() {
                                return;
                            }
                            let result = handler.call(context, method, params, frame.payload).await;
                            let mut reply = match result {
                                Ok(reply) => Frame {
                                    message: Message::Result {
                                        id,
                                        result: reply.result,
                                    },
                                    payload: reply.payload,
                                },
                                Err(error) => Frame::control(Message::Error { id, error }),
                            };
                            // 宿主回调的局部编码错误只能结束该调用，不能关闭共享写通道。
                            if validate_frame(&reply).is_err() {
                                reply = Frame::control(Message::Error {
                                    id,
                                    error: PluginFault::new(
                                        ErrorCode::Fault,
                                        "host callback response cannot be encoded",
                                    ),
                                });
                            }
                            response.send_control(reply);
                            drop(permit);
                        });
                        if shared.track_callback(parent_id, task.abort_handle()) {
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
                    Err(RpcError::Protocol)
                }
            }
            _ => Err(RpcError::Protocol),
        };
        if let Err(error) = result {
            shared.fail(error);
            return;
        }
    }
}

fn callback_allowed(method: &str, stage: Stage, permissions: &[Permission]) -> bool {
    // 未登录端点只服务声明的公开内容，不能借宿主回调取得管理资源。
    if stage == Stage::PublicManagement {
        return false;
    }
    if method == "host.log" {
        return true;
    }
    if matches!(stage, Stage::Registration | Stage::Configuration) {
        return false;
    }
    if matches!(
        method,
        "host.state.get" | "host.state.put" | "host.state.delete"
    ) {
        return true;
    }
    // 失败后的策略不得产生额外出站调用或读取账号凭据。
    if stage == Stage::Retry {
        return false;
    }
    if matches!(
        method,
        gateway_plugin_sdk::call::middleware::NEXT_METHOD
            | gateway_plugin_sdk::call::middleware::BODY_READ_METHOD
            | gateway_plugin_sdk::call::middleware::BODY_CLOSE_METHOD
    ) {
        return matches!(stage, Stage::Request | Stage::Attempt);
    }
    if stage == Stage::Maintenance
        && !matches!(
            method,
            "host.data.accounts.list"
                | "host.data.quota.get"
                | "host.groups.ensure"
                | "host.groups.change_members"
                | "host.keys.ensure"
        )
    {
        return false;
    }
    let permission = match method {
        "host.http.do"
        | "host.http.do_stream"
        | "host.http.stream_read"
        | "host.http.stream_close" => Permission::Network,
        "host.model.execute"
        | "host.model.execute_stream"
        | "host.model.stream_read"
        | "host.model.stream_close"
        | "host.models.list"
        | "host.keys.list" => Permission::Models,
        "host.auth.list" | "host.auth.get_runtime" | "host.auth.get" | "host.auth.save" => {
            Permission::Accounts
        }
        "host.affinity.lookup" => Permission::Requests,
        "host.data.accounts.list" | "host.data.quota.get"
            if matches!(
                stage,
                Stage::Management | Stage::CommandLine | Stage::Maintenance
            ) =>
        {
            Permission::Data
        }
        "host.groups.ensure" | "host.groups.change_members"
            if matches!(
                stage,
                Stage::Management | Stage::CommandLine | Stage::Maintenance
            ) =>
        {
            Permission::Groups
        }
        "host.keys.ensure"
            if matches!(
                stage,
                Stage::Management | Stage::CommandLine | Stage::Maintenance
            ) =>
        {
            Permission::Keys
        }
        _ => return false,
    };
    permissions.contains(&permission)
}
