//! 插件会话测试入口与双工传输、处理器和宿主回调夹具

mod credit;
mod deadline;
mod driver;
mod lifecycle;
mod registry;

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
    CallContext, Capability, ContributionDeclaration, Contributions, ErrorCode, Frame, Handshake,
    Message, PROTOCOL_VERSION, PluginFault, Stage,
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
                "budget" => {
                    call.host
                        .call("host.log", json!({}), vec![])
                        .await
                        .map_err(SessionError::into_plugin_fault)?;
                    Ok(CallReply::unary(
                        json!({
                            "initial_ms": call.context.timeout_ms,
                            "remaining_ms": call.host.remaining_budget().map(|value| value.as_millis()),
                        }),
                        vec![],
                    ))
                }
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
                "callback" | "next_callback" => {
                    let method = if call.method == "next_callback" {
                        NEXT_METHOD
                    } else {
                        "host.log"
                    };
                    let reply = call
                        .host
                        .call(method, call.params, call.payload)
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
                "large_result" => Ok(CallReply::unary(
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
    start_configured_session(
        handler,
        capacity,
        SessionConfig {
            maximum_stream_chunk_bytes: MAXIMUM_STREAM_CHUNK_BYTES,
            maximum_calls: 4,
            maximum_callbacks: 4,
            maximum_buffered_stream_chunks: 16,
            handshake_timeout: Duration::from_secs(1),
        },
    )
    .await
}

async fn start_configured_session<H: PluginHandler>(
    handler: H,
    capacity: usize,
    config: SessionConfig,
) -> (HostPeer, JoinHandle<Result<(), SessionError>>) {
    let (host, plugin) = tokio::io::duplex(capacity);
    let (plugin_reader, plugin_writer) = tokio::io::split(plugin);
    let task = tokio::spawn(async move {
        let session = PluginSession::accept(plugin_reader, plugin_writer, config).await?;
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
                ..
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
            version: 6,
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
        request: None,
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
