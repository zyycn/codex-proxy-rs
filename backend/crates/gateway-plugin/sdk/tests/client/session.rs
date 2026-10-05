//! 验证插件会话的二进制传输、帧长度限制与不完整消息拒绝

use std::{
    future::pending,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use gateway_plugin_sdk::{
    CallContext, Capability, ContributionDeclaration, Contributions, ErrorCode, Frame, FrameError,
    Handshake, Message, PROTOCOL_VERSION, PluginFault, Stage,
    call::middleware::{
        BODY_READ_METHOD, HANDLE_METHOD, MiddlewareBodyDisposition, MiddlewareBodyFrame,
        MiddlewareBodyFraming, MiddlewareBodyHandle, MiddlewareBodyReadResult, MiddlewareHeader,
        MiddlewareHeaderMutation, MiddlewareMount, MiddlewareNextRequest, MiddlewareNextResponse,
        MiddlewareRequestBody, MiddlewareRequestHead, MiddlewareResponseBody,
        MiddlewareResponseHead, MiddlewareTransport, NEXT_METHOD,
    },
    client::{
        CallFuture, CallReply, MiddlewarePlugin, PluginCall, PluginHandler, PluginSession,
        RequestCall, ResponseStream, SessionConfig, SessionError, read_frame, write_frame,
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf},
    task::JoinHandle,
};

const MAXIMUM_STREAM_CHUNK_BYTES: usize = 64 * 1024;

#[tokio::test]
async fn binary_payload_round_trips_without_json_encoding() {
    let frame = Frame {
        message: Message::Stream {
            id: 17,
            sequence: 2,
        },
        payload: vec![0, 255, 13, 10, 128],
    };
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &frame).await.unwrap();
    assert!(bytes.ends_with(&frame.payload));
    assert_eq!(read_frame(&mut bytes.as_slice()).await.unwrap(), frame);
}

#[tokio::test]
async fn malicious_lengths_fail_before_reading_or_allocating_payload() {
    let bytes = [0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(matches!(
        read_frame(&mut bytes.as_slice()).await,
        Err(FrameError::Length)
    ));
}

#[tokio::test]
async fn large_payload_is_exact_across_multiple_io_buffers_and_following_frames() {
    let frame = Frame {
        message: Message::Result {
            id: 1,
            result: json!({}),
        },
        payload: (0..101 * 1024 * 1024 + 13)
            .map(|index| (index % 251) as u8)
            .collect(),
    };
    let next = Frame::control(Message::Cancel { id: 3 });
    let (mut writer, mut reader) = tokio::io::duplex(16 * 1024);
    let writing = tokio::spawn(async move {
        write_frame(&mut writer, &frame).await.unwrap();
        write_frame(&mut writer, &next).await.unwrap();
        (frame, next)
    });
    let received = read_frame(&mut reader).await.unwrap();
    let following = read_frame(&mut reader).await.unwrap();
    let (original, next) = writing.await.unwrap();
    assert_eq!(received, original);
    assert_eq!(following, next);
}

#[tokio::test]
async fn truncated_large_payload_never_becomes_a_successful_message() {
    let message = serde_json::to_vec(&Message::Result {
        id: 1,
        result: json!({}),
    })
    .unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(message.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&(101_u64 * 1024 * 1024).to_be_bytes());
    bytes.extend_from_slice(&message);
    bytes.extend_from_slice(&[1, 2, 3]);
    assert!(matches!(
        read_frame(&mut bytes.as_slice()).await,
        Err(FrameError::Io(_))
    ));
}

#[tokio::test]
async fn truncated_frame_never_becomes_a_successful_message() {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &Frame::control(Message::Cancel { id: 3 }))
        .await
        .unwrap();
    bytes.pop();
    assert!(matches!(
        read_frame(&mut bytes.as_slice()).await,
        Err(FrameError::Io(_))
    ));
}

#[derive(Clone, Default)]
struct TestHandler {
    lifecycle: Arc<LifecycleState>,
    cancel_delay: Option<Duration>,
}

#[derive(Default)]
struct LifecycleState {
    cancel_started: AtomicBool,
    cancelled: AtomicUsize,
    quiesced: AtomicBool,
    shutdown: AtomicBool,
}

impl PluginHandler for TestHandler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            match call.method.as_str() {
                "echo" => Ok(CallReply::unary(call.params, call.payload)),
                "key_facts" => {
                    let result = call
                        .host
                        .key_facts(gateway_plugin_sdk::call::data::ClientKeyFactsQuery {
                            client_key_id: "key_1".into(),
                        })
                        .await?;
                    Ok(CallReply::unary(
                        serde_json::to_value(result).unwrap(),
                        vec![],
                    ))
                }
                "get_budget" => {
                    let result = call
                        .host
                        .get_key_budget(
                            gateway_plugin_sdk::call::key_budgets::GetKeyBudgetRequest {
                                client_key_id: "key_1".into(),
                            },
                        )
                        .await?;
                    Ok(CallReply::unary(
                        serde_json::to_value(result).unwrap(),
                        vec![],
                    ))
                }
                "update_budget_limits" => {
                    let result = call
                        .host
                        .update_key_budget_limits(
                            gateway_plugin_sdk::call::key_budgets::UpdateKeyBudgetLimitsRequest {
                                client_key_id: "key_1".into(),
                                daily_limit_usd: None,
                                weekly_limit_usd: Some("12.5".into()),
                            },
                        )
                        .await?;
                    Ok(CallReply::unary(
                        serde_json::to_value(result).unwrap(),
                        vec![],
                    ))
                }
                "refresh_quota" => {
                    let result = call
                        .host
                        .refresh_account_quota(gateway_plugin_sdk::call::data::QuotaFactsQuery {
                            account_id: "acct_1".into(),
                        })
                        .await?;
                    Ok(CallReply::unary(
                        serde_json::to_value(result).unwrap(),
                        vec![],
                    ))
                }
                "reset_budget" => {
                    use gateway_plugin_sdk::call::{
                        host::KeyListRequest,
                        key_budgets::{BudgetPeriod, ResetKeyBudgetRequest},
                    };
                    let keys = call
                        .host
                        .list_keys(KeyListRequest {
                            cursor: None,
                            limit: 10,
                        })
                        .await?;
                    let result = call
                        .host
                        .reset_key_budget(ResetKeyBudgetRequest {
                            client_key_id: keys.keys[0].id.clone(),
                            period: BudgetPeriod::Weekly,
                        })
                        .await?;
                    Ok(CallReply::unary(
                        serde_json::to_value(result).unwrap(),
                        vec![],
                    ))
                }
                "slow" => {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    Ok(CallReply::unary(call.params, call.payload))
                }
                "callback" => {
                    let reply = call
                        .host
                        .call("host.log", call.params, call.payload)
                        .await
                        .map_err(SessionError::into_plugin_fault)?;
                    Ok(CallReply::unary(reply.result, reply.payload))
                }
                "resource_stream" => Ok(CallReply::stream(
                    json!({}),
                    Vec::new(),
                    ResponseStream::pull(Box::new(ResourceStream(Some(call.host)))),
                )),
                "stream" => Ok(CallReply::stream(
                    json!({"stream": true}),
                    Vec::new(),
                    ResponseStream::from_chunks(vec![b"abcd".to_vec(), b"efgh".to_vec()]),
                )),
                "long_stream" => Ok(CallReply::stream(
                    json!({"stream": true}),
                    Vec::new(),
                    ResponseStream::from_chunks(vec![b"x".to_vec(); 16]),
                )),
                "oversized_stream" => Ok(CallReply::stream(
                    json!({"stream": true}),
                    Vec::new(),
                    ResponseStream::from_chunks(vec![b"abcde".to_vec()]),
                )),
                "too_many_chunks" => Ok(CallReply::stream(
                    json!({"stream": true}),
                    Vec::new(),
                    ResponseStream::from_chunks(vec![b"x".to_vec(); 17]),
                )),
                "dynamic_oversized_stream" => {
                    let (sender, stream) = ResponseStream::channel(NonZeroUsize::new(1).unwrap());
                    tokio::spawn(async move {
                        let _ = sender.send(b"abcde".to_vec()).await;
                    });
                    Ok(CallReply::stream(
                        json!({"stream": true}),
                        Vec::new(),
                        stream,
                    ))
                }
                "oversized_fault" => Err(PluginFault::new(
                    ErrorCode::Upstream,
                    "x".repeat(MAXIMUM_STREAM_CHUNK_BYTES),
                )),
                "oversized_result" => Ok(CallReply::unary(
                    Value::String("x".repeat(MAXIMUM_STREAM_CHUNK_BYTES)),
                    Vec::new(),
                )),
                "dynamic_oversized_fault" => {
                    let (sender, stream) = ResponseStream::channel(NonZeroUsize::new(1).unwrap());
                    tokio::spawn(async move {
                        let _ = sender
                            .fail(PluginFault::new(
                                ErrorCode::Upstream,
                                "x".repeat(MAXIMUM_STREAM_CHUNK_BYTES),
                            ))
                            .await;
                    });
                    Ok(CallReply::stream(
                        json!({"stream": true}),
                        Vec::new(),
                        stream,
                    ))
                }
                "pending" => pending::<Result<CallReply, PluginFault>>().await,
                _ => Err(PluginFault::new(
                    ErrorCode::Unsupported,
                    "unsupported test method",
                )),
            }
        })
    }

    fn cancel(&self, _context: &CallContext) {
        self.lifecycle.cancel_started.store(true, Ordering::Release);
        if let Some(delay) = self.cancel_delay {
            std::thread::sleep(delay);
        }
        self.lifecycle.cancelled.fetch_add(1, Ordering::Relaxed);
    }

    fn quiesce(&self) {
        self.lifecycle.quiesced.store(true, Ordering::Release);
    }

    fn shutdown(&self) {
        self.lifecycle.shutdown.store(true, Ordering::Release);
    }
}

pub(super) struct HostPeer {
    pub(super) reader: ReadHalf<DuplexStream>,
    pub(super) writer: WriteHalf<DuplexStream>,
}

pub(super) async fn start_session<H: PluginHandler>(
    handler: H,
) -> (HostPeer, JoinHandle<Result<(), SessionError>>) {
    start_session_with_capacity(handler, MAXIMUM_STREAM_CHUNK_BYTES * 2).await
}

async fn start_session_with_capacity<H: PluginHandler>(
    handler: H,
    capacity: usize,
) -> (HostPeer, JoinHandle<Result<(), SessionError>>) {
    let (host, plugin) = tokio::io::duplex(capacity);
    let (plugin_reader, plugin_writer) = tokio::io::split(plugin);
    let task = tokio::spawn(async move {
        let session = PluginSession::accept(
            plugin_reader,
            plugin_writer,
            SessionConfig {
                maximum_stream_chunk_bytes: MAXIMUM_STREAM_CHUNK_BYTES,
                maximum_calls: 4,
                maximum_callbacks: 4,
                maximum_buffered_stream_chunks: 16,
                handshake_timeout: Duration::from_secs(1),
                maximum_call_timeout: Duration::from_secs(2),
            },
        )
        .await?;
        session.run(handler).await
    });
    let (mut reader, mut writer) = tokio::io::split(host);
    write_frame(
        &mut writer,
        &Frame::control(Message::Hello {
            handshake: handshake(),
        }),
    )
    .await
    .unwrap();
    let ready = read_frame(&mut reader).await.unwrap();
    assert!(matches!(
        ready,
        Frame {
            message: Message::Ready {
                protocol_version: PROTOCOL_VERSION,
                ref incarnation,
            },
            ref payload,
        } if incarnation == "test-incarnation" && payload.is_empty()
    ));
    (HostPeer { reader, writer }, task)
}

fn middleware_contributions() -> Contributions {
    Contributions::from([(
        Capability::Middleware,
        ContributionDeclaration {
            id: "test.example.middleware".into(),
            version: 3,
            stages: vec![Stage::Request],
            input_formats: vec!["openai".into()],
            output_formats: vec!["openai".into()],
        },
    )])
}

fn middleware_plugin(map_response: bool) -> impl PluginHandler {
    MiddlewarePlugin::new(
        &middleware_contributions(),
        move |call: RequestCall| async move {
            let RequestCall {
                mut request, next, ..
            } = call;
            if map_response {
                request.body.push(b'!');
                request.remove_header("x-old");
                request.head.headers.push(MiddlewareHeader {
                    name: "x-direct".into(),
                    value: b"request".to_vec(),
                });
            }
            let mut response = next.run(request).await?;
            if map_response {
                response.remove_header("x-upstream");
                response.headers.push(MiddlewareHeader {
                    name: "x-direct-response".into(),
                    value: b"response".to_vec(),
                });
                response.body = response.body.map_frames(|mut frame| {
                    frame.payload.make_ascii_uppercase();
                    Ok(vec![frame])
                })?;
            }
            Ok(response)
        },
    )
    .unwrap()
}

#[test]
fn middleware_plugin_rejects_declarations_without_matching_handlers() {
    let declaration = middleware_contributions()
        .remove(&Capability::Middleware)
        .unwrap();
    let mut invalid = vec![Contributions::new()];
    invalid.push(Contributions::from([(
        Capability::Scheduler,
        declaration.clone(),
    )]));
    invalid.push(Contributions::from([
        (Capability::Middleware, declaration.clone()),
        (Capability::Scheduler, declaration.clone()),
    ]));
    let mut unsupported = declaration.clone();
    unsupported.version = 2;
    invalid.push(Contributions::from([(Capability::Middleware, unsupported)]));
    for stages in [
        vec![],
        vec![Stage::Registration],
        vec![Stage::Request, Stage::Request],
        vec![Stage::Request, Stage::Attempt, Stage::Request],
    ] {
        let mut invalid_stage = declaration.clone();
        invalid_stage.stages = stages;
        invalid.push(Contributions::from([(
            Capability::Middleware,
            invalid_stage,
        )]));
    }
    for contributes in invalid {
        let result = MiddlewarePlugin::new(&contributes, |call: RequestCall| async move {
            call.next.run(call.request).await
        });
        assert!(matches!(result, Err(SessionError::Configuration)));
    }
    let mut both_mounts = declaration;
    both_mounts.stages.push(Stage::Attempt);
    let contributes = Contributions::from([(Capability::Middleware, both_mounts)]);
    assert!(
        MiddlewarePlugin::new(&contributes, |call: RequestCall| async move {
            call.next.run(call.request).await
        })
        .is_ok()
    );
}

#[tokio::test]
async fn middleware_plugin_registers_without_invoking_business_handler() {
    let contributes = middleware_contributions();
    let plugin = MiddlewarePlugin::new(&contributes, |_: RequestCall| async {
        panic!("registration must not invoke middleware")
    })
    .unwrap();
    let (mut host, task) = start_session(plugin).await;
    let mut registration_context = context(1, Duration::from_secs(1));
    registration_context.stage = Stage::Registration;
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id: 1,
                method: "plugin.register".into(),
                context: registration_context,
                params: json!({}),
            },
            payload: vec![],
        },
    )
    .await
    .unwrap();
    let reply = receive(&mut host).await;
    let Message::Result { id: 1, result } = reply.message else {
        panic!("expected registration result")
    };
    let registration: gateway_plugin_sdk::call::registration::Registration =
        serde_json::from_value(result).unwrap();
    assert_eq!(registration.contributes, contributes);
    assert!(reply.payload.is_empty());
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn middleware_plugin_rejects_invalid_calls_before_business_dispatch() {
    let plugin = MiddlewarePlugin::new(&middleware_contributions(), |_: RequestCall| async {
        panic!("invalid call must not invoke middleware")
    })
    .unwrap();
    let (mut host, task) = start_session(plugin).await;
    let cases = [
        ("plugin.register", Stage::Request, json!({}), vec![]),
        ("plugin.register", Stage::Registration, json!(null), vec![]),
        (
            "plugin.register",
            Stage::Registration,
            json!({"extra":true}),
            vec![],
        ),
        ("plugin.register", Stage::Registration, json!({}), vec![1]),
        (HANDLE_METHOD, Stage::Attempt, json!({}), vec![]),
        (HANDLE_METHOD, Stage::Request, json!({}), vec![]),
        ("unknown.execute", Stage::Request, json!({}), vec![]),
    ];
    for (index, (method, stage, params, payload)) in cases.into_iter().enumerate() {
        let id = u64::try_from(index).unwrap() * 2 + 1;
        let mut call_context = middleware_context(id);
        call_context.stage = stage;
        write_frame(
            &mut host.writer,
            &Frame {
                message: Message::Call {
                    id,
                    method: method.into(),
                    context: call_context,
                    params,
                },
                payload,
            },
        )
        .await
        .unwrap();
        let reply = receive(&mut host).await;
        let Message::Error {
            id: reply_id,
            error,
        } = reply.message
        else {
            panic!("expected invalid call error")
        };
        assert_eq!(reply_id, id);
        assert_eq!(
            error.code,
            if method == "unknown.execute" {
                ErrorCode::Unsupported
            } else {
                ErrorCode::InvalidInput
            }
        );
    }
    shutdown(&mut host, task).await;
}

fn handshake() -> Handshake {
    Handshake {
        protocol_version: PROTOCOL_VERSION,
        artifact_sha256: "a".repeat(64),
        plugin_id: "test.example".into(),
        instance_id: "test-instance".into(),
        generation: 7,
        incarnation: "test-incarnation".into(),
        configuration: json!({}),

        contributes: Contributions::new(),
    }
}

pub(super) fn context(id: u64, timeout: Duration) -> CallContext {
    CallContext {
        call_id: id,
        instance_id: "test-instance".into(),
        generation: 7,
        incarnation: "test-incarnation".into(),
        stage: Stage::Request,
        timeout_ms: timeout.as_millis() as u64,
        resource_stream: false,
        resource_scope_id: format!("scope-{id}"),
        request_id: None,
        attempt_id: None,
        account_id: None,
        credential_revision: None,
    }
}

fn middleware_context(id: u64) -> CallContext {
    CallContext {
        stage: Stage::Request,
        request_id: Some("request-middleware".into()),
        ..context(id, Duration::from_secs(1))
    }
}

pub(super) fn middleware_request() -> MiddlewareRequestHead {
    MiddlewareRequestHead {
        settings_sources: serde_json::Value::Null,
        settings: serde_json::Value::Null,
        client_key_id: "fixture-key".into(),
        account_group_ids: vec!["fixture-group".into()],
        request_id: "request-middleware".into(),
        mount: MiddlewareMount::Request,
        attempt_index: None,
        operation: "generate".into(),
        protocol: "openai".into(),
        endpoint: "responses".into(),
        transport: MiddlewareTransport::HttpSse,
        provider: None,
        model: Some("public-model".into()),
        account_id: None,
        headers: vec![MiddlewareHeader {
            name: "x-old".into(),
            value: b"old".to_vec(),
        }],
    }
}

pub(super) async fn send_call(
    host: &mut HostPeer,
    id: u64,
    method: &str,
    params: Value,
    payload: Vec<u8>,
) {
    send_call_with_timeout(host, id, method, params, payload, Duration::from_secs(1)).await;
}

async fn send_call_with_timeout(
    host: &mut HostPeer,
    id: u64,
    method: &str,
    params: Value,
    payload: Vec<u8>,
    timeout: Duration,
) {
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id,
                method: method.into(),
                context: context(id, timeout),
                params,
            },
            payload,
        },
    )
    .await
    .unwrap();
}

async fn send_control(host: &mut HostPeer, message: Message) {
    write_frame(&mut host.writer, &Frame::control(message))
        .await
        .unwrap();
}

pub(super) async fn receive(host: &mut HostPeer) -> Frame {
    tokio::time::timeout(Duration::from_secs(1), read_frame(&mut host.reader))
        .await
        .expect("plugin response timed out")
        .expect("plugin response frame must be valid")
}

pub(super) async fn shutdown(host: &mut HostPeer, task: JoinHandle<Result<(), SessionError>>) {
    send_control(host, Message::Shutdown).await;
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("plugin session did not shut down")
        .expect("plugin session task panicked")
        .expect("plugin session shutdown failed");
}

#[tokio::test]
async fn typed_budget_helpers_preserve_wire_contract_and_propagate_failure_without_retry() {
    for failed in [false, true] {
        let (mut host, task) = start_session(TestHandler::default()).await;
        send_call(&mut host, 1, "reset_budget", json!({}), vec![]).await;
        let list = receive(&mut host).await;
        let Message::Callback {
            id,
            parent_id: 1,
            method,
            params,
        } = list.message
        else {
            panic!("expected Key list callback")
        };
        assert_eq!(method, "host.keys.list");
        assert_eq!(params, json!({"cursor":null,"limit":10}));
        assert!(list.payload.is_empty());
        send_control(&mut host, Message::Result { id, result: json!({"keys":[{"id":"key_1","name":"test","enabled":true}],"next_cursor":null}) }).await;
        let reset = receive(&mut host).await;
        let Message::Callback {
            id,
            parent_id: 1,
            method,
            params,
        } = reset.message
        else {
            panic!("expected budget reset callback")
        };
        assert_eq!(method, "host.keys.reset_budget");
        assert_eq!(params, json!({}));
        assert_eq!(
            serde_json::from_slice::<Value>(&reset.payload).unwrap(),
            json!({"client_key_id":"key_1","period":"weekly"})
        );
        if failed {
            send_control(
                &mut host,
                Message::Error {
                    id,
                    error: PluginFault::new(ErrorCode::Conflict, "stale instance"),
                },
            )
            .await;
            let Message::Error { id: 1, error } = receive(&mut host).await.message else {
                panic!("expected error propagated without another callback")
            };
            assert_eq!(error.code, ErrorCode::Conflict);
        } else {
            write_frame(
                &mut host.writer,
                &Frame {
                    message: Message::Result {
                        id,
                        result: json!({}),
                    },
                    payload: serde_json::to_vec(&json!({"client_key_id":"key_1"})).unwrap(),
                },
            )
            .await
            .unwrap();
            let Message::Result { id: 1, result } = receive(&mut host).await.message else {
                panic!("expected successful reset")
            };
            assert_eq!(result, json!({"client_key_id":"key_1"}));
        }
        shutdown(&mut host, task).await;
    }
}

#[tokio::test]
async fn independent_calls_complete_concurrently() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "slow", json!("slow"), vec![1]).await;
    send_call(&mut host, 3, "echo", json!("fast"), vec![3]).await;

    let fast = receive(&mut host).await;
    let slow = receive(&mut host).await;
    assert!(matches!(
        fast,
        Frame {
            message: Message::Result { id: 3, result },
            payload,
        } if result == json!("fast") && payload == [3]
    ));
    assert!(matches!(
        slow,
        Frame {
            message: Message::Result { id: 1, result },
            payload,
        } if result == json!("slow") && payload == [1]
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn completed_call_does_not_discard_a_partially_read_next_frame() {
    let next = Frame {
        message: Message::Call {
            id: 3,
            method: "echo".into(),
            context: context(3, Duration::from_secs(1)),
            params: json!({"fragmented": true}),
        },
        payload: vec![0, 255, 13, 10, 128],
    };
    let mut encoded = Vec::new();
    write_frame(&mut encoded, &next).await.unwrap();
    let payload_start = encoded.len() - next.payload.len();
    for split in [2, 6, 10, payload_start + 2] {
        tokio::time::timeout(Duration::from_secs(2), async {
            // 单字节缓冲保证前缀已经进入读取 future，而不是仍滞留在传输队列
            let (mut host, task) = start_session_with_capacity(TestHandler::default(), 1).await;
            send_call(&mut host, 1, "slow", json!("first"), Vec::new()).await;
            host.writer.write_all(&encoded[..split]).await.unwrap();
            assert!(matches!(
                receive(&mut host).await.message,
                Message::Result { id: 1, .. }
            ));
            host.writer.write_all(&encoded[split..]).await.unwrap();
            assert_eq!(
                receive(&mut host).await,
                Frame {
                    message: Message::Result {
                        id: 3,
                        result: json!({"fragmented": true})
                    },
                    payload: next.payload.clone(),
                }
            );
            shutdown(&mut host, task).await;
        })
        .await
        .expect("fragmented frame must survive concurrent call completion");
    }
}

#[tokio::test]
async fn host_callback_round_trips_while_parent_call_is_waiting() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "callback", json!({"event": "ready"}), vec![7]).await;

    let callback = receive(&mut host).await;
    let callback_id = match callback.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ref params,
        } if id.is_multiple_of(2)
            && method == "host.log"
            && params == &json!({"event": "ready"}) =>
        {
            id
        }
        message => panic!("unexpected callback frame: {message:?}"),
    };
    assert_eq!(callback.payload, [7]);
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: callback_id,
                result: json!({"recorded": true}),
            },
            payload: vec![9],
        },
    )
    .await
    .unwrap();

    let result = receive(&mut host).await;
    assert!(matches!(
        result,
        Frame {
            message: Message::Result { id: 1, result },
            payload,
        } if result == json!({"recorded": true}) && payload == [9]
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn callbacks_from_concurrent_calls_remain_correlated() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "callback", json!({"parent": 1}), vec![1]).await;
    send_call(&mut host, 3, "callback", json!({"parent": 3}), vec![3]).await;

    let mut callbacks = Vec::new();
    for _ in 0..2 {
        let frame = receive(&mut host).await;
        let Message::Callback {
            id,
            parent_id,
            method,
            params,
        } = frame.message
        else {
            panic!("expected callback")
        };
        assert_eq!(method, "host.log");
        assert_eq!(params, json!({"parent": parent_id}));
        assert_eq!(frame.payload, [parent_id as u8]);
        callbacks.push((id, parent_id));
    }
    assert!(callbacks[0].0 < callbacks[1].0);
    assert!(callbacks.iter().all(|(id, _)| id.is_multiple_of(2)));

    for (callback_id, parent_id) in callbacks.into_iter().rev() {
        write_frame(
            &mut host.writer,
            &Frame {
                message: Message::Result {
                    id: callback_id,
                    result: json!({"parent": parent_id}),
                },
                payload: vec![parent_id as u8],
            },
        )
        .await
        .unwrap();
    }
    let mut completed = Vec::new();
    for _ in 0..2 {
        let frame = receive(&mut host).await;
        let Message::Result { id, result } = frame.message else {
            panic!("expected parent result")
        };
        assert_eq!(result, json!({"parent": id}));
        assert_eq!(frame.payload, [id as u8]);
        completed.push(id);
    }
    completed.sort_unstable();
    assert_eq!(completed, [1, 3]);

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn middleware_next_and_body_mapping_reuse_callback_and_credit_flow() {
    let (mut host, task) = start_session(middleware_plugin(true)).await;
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id: 1,
                method: HANDLE_METHOD.into(),
                context: middleware_context(1),
                params: serde_json::to_value(middleware_request()).unwrap(),
            },
            payload: b"request".to_vec(),
        },
    )
    .await
    .unwrap();

    let next = receive(&mut host).await;
    let next_id = match next.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ref params,
        } if method == NEXT_METHOD => {
            let request: MiddlewareNextRequest = serde_json::from_value(params.clone()).unwrap();
            assert_eq!(request.body, MiddlewareRequestBody::Replace);
            assert!(request.protocol.is_none());
            assert_eq!(request.header_mutations.len(), 2);
            assert!(matches!(
                &request.header_mutations[0],
                MiddlewareHeaderMutation::Remove { name } if name == "x-old"
            ));
            assert!(matches!(
                &request.header_mutations[1],
                MiddlewareHeaderMutation::Append { name, value }
                    if name == "x-direct" && value == b"request"
            ));
            id
        }
        message => panic!("unexpected middleware next frame: {message:?}"),
    };
    assert_eq!(next.payload, b"request!");
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: next_id,
                result: serde_json::to_value(MiddlewareNextResponse {
                    metadata: None,
                    response: "response-1".into(),
                    protocol: "openai".into(),
                    status: 200,
                    headers: vec![MiddlewareHeader {
                        name: "x-upstream".into(),
                        value: b"old".to_vec(),
                    }],
                    body: Some(MiddlewareBodyHandle {
                        handle: "body-1".into(),
                        framing: MiddlewareBodyFraming::SseEvent,
                    }),
                })
                .unwrap(),
            },
            payload: Vec::new(),
        },
    )
    .await
    .unwrap();
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1_024,
            frames: 2,
        },
    )
    .await;

    let initial = receive(&mut host).await;
    let Message::Result { id: 1, result } = initial.message else {
        panic!("expected middleware result")
    };
    let head: MiddlewareResponseHead = serde_json::from_value(result).unwrap();
    assert_eq!(head.header_mutations.len(), 2);
    assert!(matches!(
        &head.header_mutations[0],
        MiddlewareHeaderMutation::Remove { name } if name == "x-upstream"
    ));
    assert!(matches!(
        &head.header_mutations[1],
        MiddlewareHeaderMutation::Append { name, value }
            if name == "x-direct-response" && value == b"response"
    ));
    assert!(matches!(
        head.body,
        MiddlewareResponseBody::Stream {
            framing: MiddlewareBodyFraming::SseEvent
        }
    ));

    let read = receive(&mut host).await;
    let read_id = match read.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ..
        } if method == BODY_READ_METHOD => id,
        message => panic!("unexpected middleware body read: {message:?}"),
    };
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: read_id,
                result: serde_json::to_value(MiddlewareBodyReadResult {
                    framing: MiddlewareBodyFraming::SseEvent,
                    source_id: 1,
                    eof: false,
                    terminal: true,
                })
                .unwrap(),
            },
            payload: b"data: done\n\n".to_vec(),
        },
    )
    .await
    .unwrap();
    let mapped = receive(&mut host).await;
    let Frame {
        message: Message::Stream { id: 1, sequence: 0 },
        payload,
    } = mapped
    else {
        panic!("expected mapped middleware frame")
    };
    let mapped = MiddlewareBodyFrame::decode(&payload).unwrap();
    assert_eq!(mapped.payload, b"DATA: DONE\n\n");
    assert!(!mapped.terminal);
    assert_eq!(mapped.source_id(), 1);
    assert_eq!(mapped.disposition(), MiddlewareBodyDisposition::Only);

    let eof = receive(&mut host).await;
    let eof_id = match eof.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ..
        } if method == BODY_READ_METHOD => id,
        message => panic!("unexpected middleware eof read: {message:?}"),
    };
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: eof_id,
                result: serde_json::to_value(MiddlewareBodyReadResult {
                    framing: MiddlewareBodyFraming::SseEvent,
                    source_id: 0,
                    eof: true,
                    terminal: false,
                })
                .unwrap(),
            },
            payload: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn untouched_middleware_response_transfers_opaque_body_without_reading_it() {
    let (mut host, task) = start_session(middleware_plugin(false)).await;
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id: 1,
                method: HANDLE_METHOD.into(),
                context: middleware_context(1),
                params: serde_json::to_value(middleware_request()).unwrap(),
            },
            payload: b"hidden-by-preserve".to_vec(),
        },
    )
    .await
    .unwrap();

    let next = receive(&mut host).await;
    let next_id = match next.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ref params,
        } if method == NEXT_METHOD => {
            let request: MiddlewareNextRequest = serde_json::from_value(params.clone()).unwrap();
            assert_eq!(request.body, MiddlewareRequestBody::Preserve);
            id
        }
        message => panic!("unexpected middleware next frame: {message:?}"),
    };
    assert!(next.payload.is_empty());
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: next_id,
                result: serde_json::to_value(MiddlewareNextResponse {
                    metadata: None,
                    response: "response-1".into(),
                    protocol: "openai".into(),
                    status: 200,
                    headers: Vec::new(),
                    body: Some(MiddlewareBodyHandle {
                        handle: "body-pass-through".into(),
                        framing: MiddlewareBodyFraming::RawBytes,
                    }),
                })
                .unwrap(),
            },
            payload: Vec::new(),
        },
    )
    .await
    .unwrap();
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1,
            frames: 1,
        },
    )
    .await;

    let initial = receive(&mut host).await;
    let Message::Result { id: 1, result } = initial.message else {
        panic!("expected middleware result")
    };
    let head: MiddlewareResponseHead = serde_json::from_value(result).unwrap();
    assert!(matches!(
        head.body,
        MiddlewareResponseBody::PassThrough {
            body: MiddlewareBodyHandle { ref handle, .. }
        } if handle == "body-pass-through"
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn stream_waits_for_credit_and_rejects_uncreditable_buffered_chunk_before_result() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "stream", json!({}), Vec::new()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), read_frame(&mut host.reader))
            .await
            .is_err()
    );
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 4,
            frames: 1,
        },
    )
    .await;

    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    assert!(matches!(
        receive(&mut host).await,
        Frame {
            message: Message::Stream { id: 1, sequence: 0 },
            payload,
        } if payload == b"abcd"
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), read_frame(&mut host.reader))
            .await
            .is_err()
    );
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 4,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await,
        Frame {
            message: Message::Stream { id: 1, sequence: 1 },
            payload,
        } if payload == b"efgh"
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));

    send_call(&mut host, 3, "oversized_stream", json!({}), Vec::new()).await;
    send_control(
        &mut host,
        Message::Credit {
            id: 3,
            bytes: 4,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 3,
            error: PluginFault {
                code: ErrorCode::InvalidInput,
                ..
            },
        }
    ));

    send_call(&mut host, 5, "too_many_chunks", json!({}), Vec::new()).await;
    send_control(
        &mut host,
        Message::Credit {
            id: 5,
            bytes: 4,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 5,
            error: PluginFault {
                code: ErrorCode::Capacity,
                ..
            },
        }
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn late_credit_after_a_long_stream_terminal_does_not_close_the_session() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "long_stream", json!({}), Vec::new()).await;
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    for sequence in 0..16 {
        assert!(matches!(
            receive(&mut host).await,
            Frame {
                message: Message::Stream { id: 1, sequence: observed },
                payload,
            } if observed == sequence && payload == b"x"
        ));
        send_control(
            &mut host,
            Message::Credit {
                id: 1,
                bytes: 1,
                frames: 1,
            },
        )
        .await;
    }
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));

    send_call(&mut host, 3, "echo", json!("still-ready"), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 3, result } if result == json!("still-ready")
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn malformed_late_credit_remains_a_protocol_error() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "echo", json!({}), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));

    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 0,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("protocol error did not close the session")
            .expect("plugin session task panicked"),
        Err(SessionError::Protocol)
    ));
}

#[tokio::test]
async fn dynamic_stream_ends_with_one_error_when_a_later_chunk_cannot_fit_credit() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(
        &mut host,
        1,
        "dynamic_oversized_stream",
        json!({}),
        Vec::new(),
    )
    .await;
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 4,
            frames: 1,
        },
    )
    .await;

    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End {
            id: 1,
            error: Some(PluginFault {
                code: ErrorCode::InvalidInput,
                ..
            }),
        }
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn oversized_handler_faults_are_replaced_without_closing_the_session() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "oversized_result", json!({}), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 1,
            error: PluginFault {
                code: ErrorCode::InvalidInput,
                ..
            },
        }
    ));

    send_call(&mut host, 3, "oversized_fault", json!({}), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 3,
            error: PluginFault {
                code: ErrorCode::Fault,
                ref message,
                ..
            },
        } if message == "plugin error metadata exceeds its limit"
    ));

    send_call(
        &mut host,
        5,
        "dynamic_oversized_fault",
        json!({}),
        Vec::new(),
    )
    .await;
    send_control(
        &mut host,
        Message::Credit {
            id: 5,
            bytes: 1_024,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 5, .. }
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End {
            id: 5,
            error: Some(PluginFault {
                code: ErrorCode::Fault,
                ref message,
                ..
            }),
        } if message == "plugin stream error metadata exceeds its limit"
    ));

    send_call(&mut host, 7, "echo", json!("still-ready"), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 7, result } if result == json!("still-ready")
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn call_deadline_cancels_handler_and_keeps_the_session_available() {
    let handler = TestHandler::default();
    let state = Arc::clone(&handler.lifecycle);
    let (mut host, task) = start_session(handler).await;
    send_call_with_timeout(
        &mut host,
        1,
        "pending",
        json!({}),
        Vec::new(),
        Duration::from_millis(20),
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 1,
            error: PluginFault {
                code: ErrorCode::Timeout,
                ..
            },
        }
    ));
    tokio::time::timeout(Duration::from_millis(100), async {
        while !state.cancel_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("deadline did not notify the lifecycle hook");

    send_call(&mut host, 3, "echo", json!("still-ready"), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 3, result } if result == json!("still-ready")
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn cancelling_parent_retires_callback_and_late_reply_does_not_close_session() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "callback", json!({}), Vec::new()).await;
    let callback = receive(&mut host).await;
    let Message::Callback {
        id: callback_id, ..
    } = callback.message
    else {
        panic!("expected callback")
    };

    send_control(&mut host, Message::Cancel { id: 1 }).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Cancelled { id: 1 }
    ));
    send_control(
        &mut host,
        Message::Result {
            id: callback_id,
            result: json!({}),
        },
    )
    .await;
    send_call(&mut host, 3, "echo", json!("still-ready"), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 3, result } if result == json!("still-ready")
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn blocking_cancel_hook_cannot_delay_cancelled_or_eof_settlement() {
    let handler = TestHandler {
        cancel_delay: Some(Duration::from_millis(200)),
        ..TestHandler::default()
    };
    let state = Arc::clone(&handler.lifecycle);
    let (mut host, task) = start_session(handler).await;
    send_call(&mut host, 1, "pending", json!({}), Vec::new()).await;
    send_control(&mut host, Message::Cancel { id: 1 }).await;

    let cancelled = tokio::time::timeout(Duration::from_millis(50), receive(&mut host))
        .await
        .expect("blocking lifecycle hook delayed Cancelled");
    assert!(matches!(cancelled.message, Message::Cancelled { id: 1 }));
    tokio::time::timeout(Duration::from_millis(50), async {
        while !state.cancel_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancel hook never started");

    host.writer.shutdown().await.unwrap();
    let result = tokio::time::timeout(Duration::from_millis(50), task)
        .await
        .expect("blocking lifecycle hook delayed EOF settlement")
        .expect("plugin session task panicked");
    assert!(matches!(result, Err(SessionError::Closed)));
}

#[tokio::test]
async fn quiesce_rejects_new_calls_and_shutdown_closes_the_session() {
    let handler = TestHandler::default();
    let state = Arc::clone(&handler.lifecycle);
    let (mut host, task) = start_session(handler).await;
    send_control(&mut host, Message::Quiesce).await;
    send_call(&mut host, 1, "echo", json!({}), Vec::new()).await;

    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 1,
            error: PluginFault {
                code: ErrorCode::Capacity,
                ..
            },
        }
    ));
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1,
            frames: 1,
        },
    )
    .await;
    tokio::time::timeout(Duration::from_millis(50), async {
        while !state.quiesced.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiesce hook was not signalled");

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn typed_key_and_quota_calls_keep_payloads_and_do_not_retry_failures() {
    for (entry, method, request, response) in [
        (
            "key_facts",
            "host.data.keys.get",
            json!({"client_key_id":"key_1"}),
            json!({"schema_version":1,"client_key_id":"key_1","enabled":false,"group_ids":["grp_1"]}),
        ),
        (
            "get_budget",
            "host.keys.get_budget",
            json!({"client_key_id":"key_1"}),
            json!({
                "client_key_id":"key_1", "daily_limit_usd":"10", "weekly_limit_usd":"20",
                "daily_used_usd":"1.25", "weekly_used_usd":"2.5", "daily_resets_at_ms":null, "weekly_resets_at_ms":null,
            }),
        ),
        (
            "update_budget_limits",
            "host.keys.update_budget_limits",
            json!({"client_key_id":"key_1","weekly_limit_usd":"12.5"}),
            json!({"client_key_id":"key_1"}),
        ),
        (
            "refresh_quota",
            "host.quota_observations.refresh",
            json!({"account_id":"acct_1"}),
            json!({"schema_version":1,"account_id":"acct_1","observed_at_ms":null,"windows":[]}),
        ),
    ] {
        for failed in [false, true] {
            let (mut host, task) = start_session(TestHandler::default()).await;
            send_call(&mut host, 1, entry, json!({}), vec![]).await;
            let callback = receive(&mut host).await;
            let Message::Callback {
                id,
                parent_id: 1,
                method: actual,
                params,
            } = callback.message
            else {
                panic!("expected callback");
            };
            assert_eq!(actual, method);
            assert_eq!(params, json!({}));
            assert_eq!(
                serde_json::from_slice::<Value>(&callback.payload).unwrap(),
                request
            );
            if failed {
                send_control(
                    &mut host,
                    Message::Error {
                        id,
                        error: PluginFault::new(ErrorCode::Conflict, "changed"),
                    },
                )
                .await;
                let Message::Error { id: 1, error } = receive(&mut host).await.message else {
                    panic!("expected propagated error without retry");
                };
                assert_eq!(error.code, ErrorCode::Conflict);
            } else {
                let mut response_payload = response.clone();
                if matches!(entry, "key_facts" | "refresh_quota") {
                    response_payload["future_fact"] = json!({"value":1});
                }
                write_frame(
                    &mut host.writer,
                    &Frame {
                        message: Message::Result {
                            id,
                            result: json!({}),
                        },
                        payload: serde_json::to_vec(&response_payload).unwrap(),
                    },
                )
                .await
                .unwrap();
                let Message::Result { id: 1, result } = receive(&mut host).await.message else {
                    panic!("expected projected response");
                };
                assert_eq!(result, response);
            }
            shutdown(&mut host, task).await;
        }
    }
}

struct ResourceStream(Option<gateway_plugin_sdk::client::HostClient>);
impl gateway_plugin_sdk::client::PullResponseStream for ResourceStream {
    fn next(&mut self) -> gateway_plugin_sdk::client::PullResponseFuture<'_> {
        Box::pin(async move {
            let host = self.0.take()?;
            tokio::time::sleep(Duration::from_millis(700)).await;
            Some(
                host.call("host.log", json!({}), Vec::new())
                    .await
                    .map(|_| b"live".to_vec())
                    .map_err(SessionError::into_plugin_fault),
            )
        })
    }
}

#[tokio::test]
async fn resource_stream_keeps_callbacks_and_credit_alive_after_the_initial_deadline() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    let mut context = context(1, Duration::from_millis(500));
    context.resource_stream = true;
    write_frame(
        &mut host.writer,
        &Frame::control(Message::Call {
            id: 1,
            method: "resource_stream".into(),
            context,
            params: json!({}),
        }),
    )
    .await
    .unwrap();
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 16,
            frames: 2,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    let Message::Callback {
        id, parent_id: 1, ..
    } = receive(&mut host).await.message
    else {
        panic!("stream callback must survive initial timeout");
    };
    send_control(
        &mut host,
        Message::Result {
            id,
            result: json!({}),
        },
    )
    .await;
    assert!(
        matches!(receive(&mut host).await, Frame { message: Message::Stream { id: 1, sequence: 0 }, payload } if payload == b"live")
    );
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));
    shutdown(&mut host, task).await;
}
