//! Live sideband 的 WebSocket 中继 adapter。
//!
//! 上游连接由 [`gateway_core::live::LiveGateway`] 用钉住账号的凭据建立；
//! 本模块只做帧形态映射（Text/Binary 双向、Ping 本地应答、Close 投影），
//! 不解释帧内容。

use std::time::Duration;

use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use gateway_core::lifecycle::{CancellationToken, ConnectionGuard};
use gateway_core::live::{LiveClose, LiveFrame, LiveRelay, LiveSidebandRequest, LiveSidebandStyle};

use super::LiveErrorShape;
use crate::{
    ApiState,
    openai::{
        auth::{authenticate_client, client_access_error_response},
        error::runtime_unavailable_response,
        live::{
            filter_protocol_headers, live_error_response, offered_subprotocols,
            realtime_unsupported_response,
        },
    },
};

const SIDEBAND_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn sideband(
    State(state): State<ApiState>,
    axum::extract::Path(call_id): axum::extract::Path<String>,
    uri: Uri,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let style = if uri.path().contains("/realtime/calls/") {
        LiveSidebandStyle::RealtimeCalls
    } else {
        LiveSidebandStyle::Live
    };
    open_sideband(state, call_id, style, headers, upgrade).await
}

/// `GET /v1/realtime`：携带 `call_id` 时加入既有通话；标准 realtime
/// WebSocket（`?model=…`）在本网关的 Codex OAuth 上游上不可用，返回 501。
pub(crate) async fn realtime_get(
    State(state): State<ApiState>,
    uri: Uri,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let call_id = uri
        .query()
        .and_then(|query| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix("call_id="))
        })
        .map(percent_decode);
    match call_id.filter(|call_id| !call_id.is_empty()) {
        Some(call_id) => {
            open_sideband(
                state,
                call_id,
                LiveSidebandStyle::RealtimeQuery,
                headers,
                upgrade,
            )
            .await
        }
        None => realtime_unsupported_response("Direct Realtime WebSocket"),
    }
}

fn percent_decode(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(byte) = bytes
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok())
        {
            output.push(byte);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

async fn open_sideband(
    state: ApiState,
    call_id: String,
    style: LiveSidebandStyle,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Ok(upgrade) = upgrade else {
        return upgrade_required_response();
    };
    let service = state.openai();
    let client = match authenticate_client(service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let Some(gateway) = service.live_gateway() else {
        return realtime_unsupported_response("Codex live sideband");
    };
    // sideband 是长连接：在拨号前注册连接 guard，drain 才能统计并等待它关闭。
    let connection_guard = match service.try_register_connection() {
        Ok(guard) => guard,
        Err(_) => return runtime_unavailable_response().into_response(),
    };
    let cancellation = service.lifecycle().cancellation();
    let opening = gateway.open_sideband(LiveSidebandRequest {
        call_id: &call_id,
        client_api_key_id: client.policy().key_id(),
        account_scope: client.policy().account_scope(),
        style,
        protocol_headers: filter_protocol_headers(&headers)
            .into_iter()
            .map(|header| {
                (
                    header.name().to_ascii_lowercase(),
                    String::from_utf8_lossy(header.value()).into_owned(),
                )
            })
            .collect(),
        subprotocols: offered_subprotocols(&headers),
    });
    let result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return runtime_unavailable_response().into_response(),
        result = opening => result,
    };
    let relay = match result {
        Ok(relay) => relay,
        Err(error) => {
            return super::http::live_gateway_error_response(LiveErrorShape::Realtime, &error);
        }
    };
    serve_sideband(upgrade, relay, connection_guard, cancellation)
}

fn upgrade_required_response() -> Response {
    let mut response = live_error_response(
        LiveErrorShape::Realtime,
        StatusCode::UPGRADE_REQUIRED,
        "WebSocket upgrade required",
        "websocket_upgrade_required",
    );
    response.headers_mut().insert(
        header::UPGRADE,
        header::HeaderValue::from_static("websocket"),
    );
    response
}

fn serve_sideband(
    mut upgrade: WebSocketUpgrade,
    relay: LiveRelay,
    connection_guard: Box<dyn ConnectionGuard>,
    cancellation: CancellationToken,
) -> Response {
    // 上游已按客户端 offer 协商子协议；只有客户端确实 offer 过才会命中。
    if let Some(subprotocol) = relay.subprotocol.clone() {
        upgrade = upgrade.protocols([subprotocol]);
    }
    upgrade
        .max_message_size(usize::MAX)
        .max_frame_size(usize::MAX)
        .on_upgrade(move |socket| async move {
            relay_sideband(socket, relay, connection_guard, cancellation).await;
        })
}

/// 双向帧中继；任一侧结束即投影关闭。
///
/// 传输中断只结束中继：call 绑定的释放由中继携带的 guard 完成，绑定本身
/// 保留到会话 TTL，官方 FramelessBidi 客户端会重连同一 call。
async fn relay_sideband(
    mut client: WebSocket,
    mut relay: LiveRelay,
    connection_guard: Box<dyn ConnectionGuard>,
    cancellation: CancellationToken,
) {
    // guard 覆盖整个中继生命周期，drain 等待其释放。
    let _connection_guard = connection_guard;
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => {},
        () = relay_frames(&mut client, &mut relay) => {},
    }
    // 写入或关闭被取消时直接丢弃传输端，避免清理再次阻塞 guard 释放
    drop(relay);
    drop(_connection_guard);
}

async fn relay_frames(client: &mut WebSocket, relay: &mut LiveRelay) {
    let mut close_forwarded = false;
    loop {
        tokio::select! {
            client_message = client.recv() => match client_message {
                Some(Ok(message)) => {
                    match message {
                        Message::Text(text) => {
                            let frame = LiveFrame::Text(Bytes::copy_from_slice(text.as_bytes()));
                            if relay.send_frame(frame).await.is_err() {
                                break;
                            }
                        }
                        Message::Binary(binary) => {
                            if relay.send_frame(LiveFrame::Binary(binary)).await.is_err() {
                                break;
                            }
                        }
                        // Ping 由本端应答（上游 Ping 同理），不进入对端。
                        Message::Ping(payload) => {
                            if client
                                .send(Message::Pong(payload))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Message::Pong(_) => {}
                        Message::Close(close) => {
                            let close = close.map(|frame| LiveClose {
                                code: frame.code,
                                reason: frame.reason.to_string(),
                            });
                            let _ = tokio::time::timeout(SIDEBAND_CLOSE_TIMEOUT, relay.close(close)).await;
                            return;
                        }
                    }
                }
                Some(Err(_)) | None => break,
            },
            upstream_frame = relay.next_frame() => match upstream_frame {
                Some(LiveFrame::Text(payload)) => {
                    if client
                        .send(Message::Text(String::from_utf8_lossy(&payload).into_owned().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(LiveFrame::Binary(payload)) => {
                    if client.send(Message::Binary(payload)).await.is_err() {
                        break;
                    }
                }
                Some(LiveFrame::Ping(payload)) => {
                    if relay.send_frame(LiveFrame::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(LiveFrame::Pong(_)) => {}
                Some(LiveFrame::Close(close)) => {
                    let _ = tokio::time::timeout(SIDEBAND_CLOSE_TIMEOUT, client.send(Message::Close(close.map(|close| CloseFrame {
                        code: close.code,
                        reason: close.reason.into(),
                    })))).await;
                    close_forwarded = true;
                    break;
                }
                None => break,
            },
        }
    }
    if !close_forwarded {
        // 异常中断按观测到的方向投影为正常关闭；原因不透出给客户端。
        let _ = tokio::time::timeout(
            SIDEBAND_CLOSE_TIMEOUT,
            client.send(Message::Close(Some(CloseFrame {
                code: 1000,
                reason: "".into(),
            }))),
        )
        .await;
    }
    let _ = tokio::time::timeout(SIDEBAND_CLOSE_TIMEOUT, relay.close(None)).await;
}
