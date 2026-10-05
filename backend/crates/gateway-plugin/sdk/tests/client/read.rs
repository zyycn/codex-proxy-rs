//! 验证异步读取在取消和背压后保留尚未交付的分块与消息

use super::session;
use gateway_plugin_sdk::{
    ErrorCode, Frame, Message, Stage,
    call::{
        host::HttpRequest,
        middleware::{BODY_FACTS_METHOD, BODY_READ_METHOD, HANDLE_METHOD, NEXT_METHOD},
        upstream_adapter::UpstreamWebSocketRequest,
    },
    client::{
        CallFuture, CallReply, HttpCall, HttpFrame, HttpResponse, MiddlewareInput,
        MiddlewareOutput, PluginCall, PluginHandler, RequestCall, UpstreamWebSocketUpgrade,
        WebSocketCall, write_frame,
    },
};
use serde_json::{Value, json};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::Notify;

#[derive(Default)]
struct Signals {
    cancel: Notify,
    dropped: Notify,
    resume: Notify,
}
struct Reader(Arc<Signals>);
impl PluginHandler for Reader {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            let bytes = if call.method == "http" {
                let mut response = call
                    .host
                    .http(
                        HttpRequest {
                            method: "GET".into(),
                            url: "https://example.test/stream".into(),
                            headers: vec![],
                        },
                        vec![],
                    )
                    .await?;
                self.0.interrupt(response.body.read()).await;
                response.body.read().await?.unwrap()
            } else {
                let UpstreamWebSocketUpgrade::Connected { connection, .. } = call
                    .host
                    .upstream_websocket(UpstreamWebSocketRequest {
                        path: "responses".into(),
                        query: vec![],
                        headers: vec![],
                    })
                    .await?
                else {
                    panic!("握手失败")
                };
                self.0.interrupt(connection.read()).await;
                connection.read().await?.unwrap().1
            };
            Ok(CallReply::unary(json!({}), bytes))
        })
    }
}
async fn callback_id(host: &mut session::HostPeer, expected: &str) -> u64 {
    let frame = session::receive(host).await;
    let Message::Callback { id, method, .. } = frame.message else {
        panic!("没有收到回调：{:?}", frame.message)
    };
    assert_eq!(method, expected);
    id
}
async fn reply(host: &mut session::HostPeer, id: u64, result: Value, bytes: &[u8]) {
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result { id, result },
            payload: bytes.into(),
        },
    )
    .await
    .unwrap();
}
async fn verify_cancelled_read(method: &str) {
    let signals = Arc::new(Signals::default());
    let (mut host, task) = session::start_session(Reader(signals.clone())).await;
    session::send_call(&mut host, 1, method, json!({}), vec![]).await;
    let (open, read, status, stream, data) = if method == "http" {
        (
            "host.http.do_stream",
            "host.http.stream_read",
            200,
            json!("body"),
            json!({"eof":false}),
        )
    } else {
        (
            "host.upstream.websocket.open",
            "host.upstream.websocket.read",
            101,
            Value::Null,
            json!({"eof":false,"kind":"text"}),
        )
    };
    let id = callback_id(&mut host, open).await;
    reply(
        &mut host,
        id,
        json!({"status":status,"headers":[],"stream":stream}),
        b"",
    )
    .await;
    let first = callback_id(&mut host, read).await;
    signals.cancel.notify_one();
    signals.dropped.notified().await;
    // 已取消的读取随后成功，下一次 read 应恢复同一读取结果
    reply(&mut host, first, data.clone(), b"first").await;
    signals.resume.notify_one();
    let next = session::receive(&mut host).await;
    let result = if let Message::Callback { id, method, .. } = next.message {
        assert_eq!(method, read);
        reply(&mut host, id, data, b"second").await;
        session::receive(&mut host).await
    } else {
        next
    };
    assert!(matches!(result.message, Message::Result { id: 1, .. }));
    session::shutdown(&mut host, task).await;
    assert_eq!(
        result.payload, b"first",
        "取消 read 的等待不应丢弃第一段数据"
    );
}
#[tokio::test]
async fn http_read_cancellation_keeps_first_chunk() {
    verify_cancelled_read("http").await;
}
#[tokio::test]
async fn upstream_websocket_cancellation_keeps_first_message() {
    verify_cancelled_read("websocket").await;
}

impl Signals {
    async fn interrupt(&self, read: impl Future) {
        tokio::select! {
            _ = self.cancel.notified() => {},
            _ = read => panic!("未取消前不得返回数据"),
        }
        self.dropped.notify_one();
        self.resume.notified().await;
    }
}

#[derive(Clone, Copy)]
enum Action {
    HttpRead,
    HttpForward,
    WebSocketRead,
    WebSocketTransfer,
    ModelFacts,
}

struct MiddlewareReader {
    signals: Arc<Signals>,
    action: Action,
}

impl PluginHandler for MiddlewareReader {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            let bytes = match self.action {
                Action::HttpRead | Action::HttpForward => {
                    let mut call = HttpCall::decode(call)?;
                    self.signals.interrupt(call.request.body.read()).await;
                    if matches!(self.action, Action::HttpForward) {
                        return HttpResponse::new(200, call.request.body).into_reply();
                    }
                    let Some(HttpFrame::Data(bytes)) = call.request.body.read().await? else {
                        panic!("应保留数据帧")
                    };
                    bytes
                }
                Action::WebSocketRead | Action::WebSocketTransfer => {
                    let mut call = WebSocketCall::decode(call)?;
                    self.signals.interrupt(call.message.payload.read()).await;
                    if matches!(self.action, Action::WebSocketTransfer) {
                        return Some(call.message).encode();
                    }
                    call.message.payload.read().await?.unwrap()
                }
                Action::ModelFacts => {
                    let call = RequestCall::decode(call)?;
                    let response = call.next.run(call.request).await?;
                    let mut body = response.body.with_facts()?;
                    self.signals.interrupt(body.read()).await;
                    let frame = body.read().await?.unwrap();
                    assert_eq!(frame.source_id(), 1);
                    assert!(frame.facts.is_none());
                    frame.payload
                }
            };
            Ok(CallReply::unary(json!({}), bytes))
        })
    }
}

async fn start_middleware(
    action: Action,
) -> (
    session::HostPeer,
    tokio::task::JoinHandle<Result<(), gateway_plugin_sdk::client::SessionError>>,
    Arc<Signals>,
) {
    let signals = Arc::new(Signals::default());
    let (mut host, task) = session::start_session(MiddlewareReader {
        signals: signals.clone(),
        action,
    })
    .await;
    let (stage, params) = match action {
        Action::HttpRead | Action::HttpForward => (
            Stage::Http,
            json!({"settings_sources":null,"request_id":"request", "call_id":"http-call", "parent_call_id":null, "request": {
                "settings":null,
                "method":"POST", "uri":"/test", "version":"HTTP/1.1",
                "headers":[], "timeout_ms":null, "body":{"kind":"handle","handle":"body"}
            }}),
        ),
        Action::WebSocketRead | Action::WebSocketTransfer => (
            Stage::WebSocket,
            json!({"connection_id":"connection", "direction":"incoming", "headers":[],
                "message":{"kind":{"kind":"text"}, "payload":{"kind":"handle","handle":"body"}}
            }),
        ),
        Action::ModelFacts => (
            Stage::Request,
            serde_json::to_value(session::middleware_request()).unwrap(),
        ),
    };
    let mut context = session::context(1, Duration::from_secs(2));
    context.stage = stage;
    context.request_id = params["request_id"].as_str().map(str::to_owned);
    write_frame(
        &mut host.writer,
        &Frame::control(Message::Call {
            id: 1,
            method: HANDLE_METHOD.into(),
            context,
            params,
        }),
    )
    .await
    .unwrap();
    (host, task, signals)
}

#[tokio::test]
async fn middleware_http_and_websocket_reads_resume_the_pending_chunk() {
    for action in [Action::HttpRead, Action::WebSocketRead] {
        let (mut host, task, signals) = start_middleware(action).await;
        let method = if matches!(action, Action::HttpRead) {
            gateway_plugin_sdk::call::middleware::http::BODY_READ_METHOD
        } else {
            BODY_READ_METHOD
        };
        let id = callback_id(&mut host, method).await;
        signals.cancel.notify_one();
        signals.dropped.notified().await;
        reply(&mut host, id, json!({"eof":false}), b"first").await;
        signals.resume.notify_one();
        let frame = session::receive(&mut host).await;
        assert!(matches!(frame.message, Message::Result { id: 1, .. }));
        assert_eq!(frame.payload, b"first");
        session::shutdown(&mut host, task).await;
    }
}

#[tokio::test]
async fn model_frame_survives_cancellation_while_fetching_its_facts() {
    let (mut host, task, signals) = start_middleware(Action::ModelFacts).await;
    let id = callback_id(&mut host, NEXT_METHOD).await;
    reply(
        &mut host,
        id,
        json!({"response":"response", "protocol":"openai", "status":200,
            "body":{"handle":"body", "framing":"sse_event"}}),
        b"",
    )
    .await;
    let id = callback_id(&mut host, BODY_READ_METHOD).await;
    reply(
        &mut host,
        id,
        json!({"framing":"sse_event", "source_id":1, "eof":false, "terminal":false}),
        b"first",
    )
    .await;
    let id = callback_id(&mut host, BODY_FACTS_METHOD).await;
    signals.cancel.notify_one();
    signals.dropped.notified().await;
    reply(&mut host, id, json!({"present":false}), b"").await;
    signals.resume.notify_one();
    let frame = session::receive(&mut host).await;
    assert!(matches!(frame.message, Message::Result { id: 1, .. }));
    assert_eq!(frame.payload, b"first");
    session::shutdown(&mut host, task).await;
}

#[tokio::test]
async fn http_forwarding_retains_a_pending_read_under_output_backpressure() {
    let (mut host, task, signals) = start_middleware(Action::HttpForward).await;
    let id = callback_id(
        &mut host,
        gateway_plugin_sdk::call::middleware::http::BODY_READ_METHOD,
    )
    .await;
    signals.cancel.notify_one();
    signals.dropped.notified().await;
    reply(&mut host, id, json!({"eof":false}), b"first").await;
    signals.resume.notify_one();
    write_frame(
        &mut host.writer,
        &Frame::control(Message::Credit {
            id: 1,
            bytes: 1024,
            frames: 2,
        }),
    )
    .await
    .unwrap();
    let frame = session::receive(&mut host).await;
    assert!(
        matches!(frame.message, Message::Result { id: 1, result } if result["body"]["kind"] == "stream")
    );
    let frame = session::receive(&mut host).await;
    assert!(matches!(
        frame.message,
        Message::Stream { id: 1, sequence: 0 }
    ));
    assert_eq!(frame.payload, b"\0first");
    let id = callback_id(
        &mut host,
        gateway_plugin_sdk::call::middleware::http::BODY_READ_METHOD,
    )
    .await;
    reply(&mut host, id, json!({"eof":true}), b"").await;
    assert!(matches!(
        session::receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));
    session::shutdown(&mut host, task).await;
}

#[tokio::test]
async fn pending_http_read_is_split_when_credit_arrives_after_the_callback() {
    let (mut host, task, signals) = start_middleware(Action::HttpForward).await;
    let id = callback_id(
        &mut host,
        gateway_plugin_sdk::call::middleware::http::BODY_READ_METHOD,
    )
    .await;
    signals.cancel.notify_one();
    signals.dropped.notified().await;
    let expected = vec![b'a'; 8192];
    reply(&mut host, id, json!({"eof":false}), &expected).await;
    signals.resume.notify_one();
    write_frame(
        &mut host.writer,
        &Frame::control(Message::Credit {
            id: 1,
            bytes: 1024,
            frames: 2,
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        session::receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    let mut body = Vec::new();
    loop {
        let frame = session::receive(&mut host).await;
        match frame.message {
            Message::Stream { id: 1, .. } => {
                assert!(frame.payload.len() <= 1024);
                assert_eq!(frame.payload[0], 0);
                body.extend_from_slice(&frame.payload[1..]);
                write_frame(
                    &mut host.writer,
                    &Frame::control(Message::Credit {
                        id: 1,
                        bytes: frame.payload.len() as u32,
                        frames: 1,
                    }),
                )
                .await
                .unwrap();
            }
            Message::Callback { id, method, .. } => {
                assert_eq!(
                    method,
                    gateway_plugin_sdk::call::middleware::http::BODY_READ_METHOD
                );
                assert_eq!(body, expected);
                reply(&mut host, id, json!({"eof":true}), b"").await;
            }
            Message::End { id: 1, error: None } => break,
            message => panic!("unexpected stream message: {message:?}"),
        }
    }
    assert_eq!(body, expected);
    session::shutdown(&mut host, task).await;
}

#[tokio::test]
async fn websocket_transfer_rejects_a_payload_with_a_pending_read() {
    let (mut host, task, signals) = start_middleware(Action::WebSocketTransfer).await;
    let _id = callback_id(&mut host, BODY_READ_METHOD).await;
    signals.cancel.notify_one();
    signals.dropped.notified().await;
    signals.resume.notify_one();
    assert!(matches!(session::receive(&mut host).await.message,
        Message::Error { id: 1, error } if error.code == ErrorCode::InvalidInput));
    session::shutdown(&mut host, task).await;
}
