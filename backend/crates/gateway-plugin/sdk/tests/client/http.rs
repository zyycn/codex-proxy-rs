//! 验证插件受管 HTTP 与 WebSocket 客户端的读取、关闭和取消行为

use super::session::{HostPeer, receive, send_call, shutdown, start_session};
use gateway_plugin_sdk::client::{CallFuture, CallReply, PluginCall, PluginHandler, write_frame};
use gateway_plugin_sdk::{ErrorCode, Frame, Message};
use gateway_plugin_sdk::{
    call::{
        host::{AccountHttpRequest, HttpRequest},
        upstream_adapter::{UpstreamHttpRequest, UpstreamWebSocketRequest, WebSocketMessageKind},
    },
    client::UpstreamWebSocketUpgrade,
};
use serde_json::{Value, json};

struct NetworkHandler;

impl PluginHandler for NetworkHandler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            if call.method.starts_with("websocket") {
                let upgrade = call
                    .host
                    .upstream_websocket(UpstreamWebSocketRequest {
                        path: "responses".into(),
                        query: vec![],
                        headers: vec![],
                    })
                    .await?;
                let UpstreamWebSocketUpgrade::Connected { connection, .. } = upgrade else {
                    let UpstreamWebSocketUpgrade::Rejected { status, body, .. } = upgrade else {
                        unreachable!()
                    };
                    return Ok(CallReply::unary(json!({"status":status}), body));
                };
                if call.method == "websocket_cancel_close" {
                    {
                        let close = connection.close();
                        tokio::pin!(close);
                        let first = std::future::poll_fn(|context| {
                            std::task::Poll::Ready(close.as_mut().poll(context))
                        })
                        .await;
                        assert!(first.is_pending());
                    }
                    connection.close().await?;
                    return Ok(CallReply::unary(json!({}), vec![]));
                }
                if call.method == "websocket_close_during_read" {
                    let read = connection.read();
                    tokio::pin!(read);
                    let first = std::future::poll_fn(|context| {
                        std::task::Poll::Ready(read.as_mut().poll(context))
                    })
                    .await;
                    assert!(first.is_pending());
                    assert_eq!(
                        connection.read().await.unwrap_err().code,
                        ErrorCode::Capacity
                    );
                    connection.close().await?;
                    assert!(read.await?.is_none());
                    return Ok(CallReply::unary(json!({}), vec![]));
                }
                if call.method == "websocket_duplex" {
                    let (message, ()) = tokio::try_join!(
                        connection.read(),
                        connection.send(WebSocketMessageKind::Text, b"control".to_vec())
                    )?;
                    tokio::try_join!(connection.close(), connection.close())?;
                    return Ok(CallReply::unary(json!({}), message.unwrap().1));
                }
                connection
                    .send(WebSocketMessageKind::Text, b"request".to_vec())
                    .await?;
                let (kind, body) = connection.read().await?.unwrap();
                assert_eq!(kind, WebSocketMessageKind::Text);
                assert!(connection.read().await?.is_none());
                assert!(connection.read().await?.is_none());
                connection.close().await?;
                assert!(connection.send(kind, vec![]).await.is_err());
                return Ok(CallReply::unary(json!({}), body));
            }
            let response = if call.method == "http" {
                call.host
                    .http(
                        HttpRequest {
                            method: "POST".into(),
                            url: "https://example.test/responses".into(),
                            headers: vec![],
                        },
                        call.payload,
                    )
                    .await?
            } else if call.method == "account" {
                call.host
                    .account_http(
                        AccountHttpRequest {
                            account_id: "acct_one".into(),
                            credential_revision: 7,
                            request: HttpRequest {
                                method: "POST".into(),
                                url: "https://example.test/responses".into(),
                                headers: vec![],
                            },
                        },
                        call.payload,
                    )
                    .await?
            } else {
                call.host
                    .upstream_http(
                        UpstreamHttpRequest {
                            method: "POST".into(),
                            path: "responses".into(),
                            query: vec![],
                            headers: vec![],
                        },
                        call.payload,
                    )
                    .await?
            };
            assert_eq!(response.status, 200);
            if call.params["collect"] == true {
                return Ok(CallReply::unary(json!({}), response.body.collect(3).await?));
            }
            if call.params["metadata"] == true {
                assert_eq!(response.http_version, "HTTP/1.1");
                assert_eq!(
                    response.headers,
                    vec![
                        ("x-bytes".to_owned(), vec![0xff]),
                        ("x-bytes".to_owned(), b"other".to_vec())
                    ]
                );
            }
            let mut body = response.body;
            let bytes = body.read().await?.unwrap();
            assert!(body.read().await?.is_none());
            assert!(body.read().await?.is_none());
            if call.params["metadata"] == true {
                assert_eq!(body.trailers(), &[("x-tail".to_owned(), vec![0xfe])]);
            }
            body.close().await?;
            body.close().await?;
            Ok(CallReply::unary(json!({}), bytes))
        })
    }
}

async fn reply_callback(
    host: &mut HostPeer,
    expected: &str,
    result: Value,
    payload: Vec<u8>,
) -> Frame {
    let frame = receive(host).await;
    let Message::Callback { id, method, .. } = &frame.message else {
        panic!("expected callback")
    };
    assert_eq!(method, expected);
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result { id: *id, result },
            payload,
        },
    )
    .await
    .unwrap();
    frame
}

#[tokio::test]
async fn http_response_objects_share_pull_semantics_and_retire_eof_handles() {
    for method in ["http", "account", "upstream"] {
        let (mut host, task) = start_session(NetworkHandler).await;
        send_call(
            &mut host,
            1,
            method,
            json!({"metadata":true}),
            b"request".to_vec(),
        )
        .await;
        let prefix = if method != "upstream" {
            "host.http"
        } else {
            "host.upstream.http"
        };
        let open_method = if method == "account" {
            gateway_plugin_sdk::call::host::ACCOUNT_HTTP.to_owned()
        } else {
            format!("{prefix}.do_stream")
        };
        let frame = reply_callback(
            &mut host,
            &open_method,
            json!({"status":200,"http_version":"HTTP/1.1","trailers":[],"headers":[["x-bytes",[255]],["x-bytes",b"other".to_vec()]],"stream":"private-handle"}),
            vec![],
        )
        .await;
        assert_eq!(frame.payload, b"request");
        if method == "account" {
            assert!(matches!(frame.message, Message::Callback { params, .. }
                if params["account_id"] == "acct_one" && params["credential_revision"] == 7));
        }
        let frame = reply_callback(
            &mut host,
            &format!("{prefix}.stream_read"),
            json!({"eof":false}),
            b"abc".to_vec(),
        )
        .await;
        assert!(
            matches!(frame.message, Message::Callback { params, .. } if params == json!({"stream":"private-handle","maximum_bytes":65536}))
        );
        reply_callback(
            &mut host,
            &format!("{prefix}.stream_read"),
            json!({"eof":true,"trailers":[["x-tail",[254]]]}),
            vec![],
        )
        .await;
        let result = receive(&mut host).await;
        assert!(matches!(result.message, Message::Result { id: 1, .. }));
        assert_eq!(result.payload, b"abc");
        shutdown(&mut host, task).await;
    }
}

#[tokio::test]
async fn bounded_collection_closes_the_host_stream_before_reporting_overflow() {
    let (mut host, task) = start_session(NetworkHandler).await;
    send_call(&mut host, 1, "upstream", json!({"collect":true}), vec![]).await;
    reply_callback(
        &mut host,
        "host.upstream.http.do_stream",
        json!({"status":200,"http_version":"HTTP/1.1","trailers":[],"headers":[],"stream":"private-handle"}),
        vec![],
    )
    .await;
    reply_callback(
        &mut host,
        "host.upstream.http.stream_read",
        json!({"eof":false}),
        b"abcd".to_vec(),
    )
    .await;
    reply_callback(
        &mut host,
        "host.upstream.http.stream_close",
        json!({}),
        vec![],
    )
    .await;
    assert!(
        matches!(receive(&mut host).await.message, Message::Error { id: 1, error } if error.code == ErrorCode::Capacity)
    );
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn websocket_session_handles_messages_eof_and_http_rejection() {
    let (mut host, task) = start_session(NetworkHandler).await;
    send_call(&mut host, 1, "websocket", json!({}), vec![]).await;
    reply_callback(
        &mut host,
        "host.upstream.websocket.open",
        json!({"status":101,"http_version":"HTTP/1.1","trailers":[],"headers":[],"stream":null}),
        vec![],
    )
    .await;
    let sent = reply_callback(&mut host, "host.upstream.websocket.send", json!({}), vec![]).await;
    assert_eq!(sent.payload, b"request");
    reply_callback(
        &mut host,
        "host.upstream.websocket.read",
        json!({"eof":false,"kind":"text"}),
        b"response".to_vec(),
    )
    .await;
    reply_callback(
        &mut host,
        "host.upstream.websocket.read",
        json!({"eof":true,"kind":null}),
        vec![],
    )
    .await;
    let response = receive(&mut host).await;
    assert!(matches!(response.message, Message::Result { id: 1, .. }));
    assert_eq!(response.payload, b"response");
    send_call(&mut host, 3, "websocket", json!({}), vec![]).await;
    reply_callback(
        &mut host,
        "host.upstream.websocket.open",
        json!({"status":401,"http_version":"HTTP/1.1","trailers":[],"headers":[],"stream":null}),
        b"unauthorized".to_vec(),
    )
    .await;
    let response = receive(&mut host).await;
    assert!(
        matches!(response.message, Message::Result { id: 3, result } if result == json!({"status":401}))
    );
    assert_eq!(response.payload, b"unauthorized");
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn websocket_send_and_read_callbacks_can_be_in_flight_together() {
    let (mut host, task) = start_session(NetworkHandler).await;
    send_call(&mut host, 1, "websocket_duplex", json!({}), vec![]).await;
    reply_callback(
        &mut host,
        "host.upstream.websocket.open",
        json!({"status":101,"http_version":"HTTP/1.1","trailers":[],"headers":[],"stream":null}),
        vec![],
    )
    .await;
    let mut read_id = None;
    for _ in 0..2 {
        let frame = receive(&mut host).await;
        let Message::Callback { id, method, .. } = frame.message else {
            panic!("expected callback");
        };
        if method == "host.upstream.websocket.read" {
            read_id = Some(id);
        } else {
            assert_eq!(method, "host.upstream.websocket.send");
            assert_eq!(frame.payload, b"control");
            write_frame(
                &mut host.writer,
                &Frame {
                    message: Message::Result {
                        id,
                        result: json!({}),
                    },
                    payload: vec![],
                },
            )
            .await
            .unwrap();
        }
    }
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: read_id.unwrap(),
                result: json!({"eof":false,"kind":"text"}),
            },
            payload: b"acknowledged".to_vec(),
        },
    )
    .await
    .unwrap();
    reply_callback(
        &mut host,
        "host.upstream.websocket.close",
        json!({}),
        vec![],
    )
    .await;
    let response = receive(&mut host).await;
    assert!(matches!(response.message, Message::Result { id: 1, .. }));
    assert_eq!(response.payload, b"acknowledged");
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn websocket_close_can_be_retried_after_cancelling_its_future() {
    let (mut host, task) = start_session(NetworkHandler).await;
    send_call(&mut host, 1, "websocket_cancel_close", json!({}), vec![]).await;
    reply_callback(
        &mut host,
        "host.upstream.websocket.open",
        json!({"status":101,"http_version":"HTTP/1.1","trailers":[],"headers":[],"stream":null}),
        vec![],
    )
    .await;
    // 第一条关闭已发送，但等待其结果的 future 已取消；迟到回包不应中断重试
    for _ in 0..2 {
        reply_callback(
            &mut host,
            "host.upstream.websocket.close",
            json!({}),
            vec![],
        )
        .await;
    }
    let response = receive(&mut host).await;
    assert!(matches!(response.message, Message::Result { id: 1, .. }));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn websocket_close_does_not_wait_for_a_pending_reader() {
    let (mut host, task) = start_session(NetworkHandler).await;
    send_call(
        &mut host,
        1,
        "websocket_close_during_read",
        json!({}),
        vec![],
    )
    .await;
    reply_callback(
        &mut host,
        "host.upstream.websocket.open",
        json!({"status":101,"http_version":"HTTP/1.1","trailers":[],"headers":[],"stream":null}),
        vec![],
    )
    .await;
    let Message::Callback { id, method, .. } = receive(&mut host).await.message else {
        panic!("应发出读取回调")
    };
    assert_eq!(method, "host.upstream.websocket.read");
    reply_callback(
        &mut host,
        "host.upstream.websocket.close",
        json!({}),
        vec![],
    )
    .await;
    write_frame(
        &mut host.writer,
        &Frame::control(Message::Result {
            id,
            result: json!({"eof":true,"kind":null}),
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    shutdown(&mut host, task).await;
}
