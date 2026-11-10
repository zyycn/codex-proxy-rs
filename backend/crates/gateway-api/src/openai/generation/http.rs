//! 多协议 HTTP 生成与计数的执行交付和连接生命周期

use crate::request_context::request_client_context;

use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::Extension,
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode,
        header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::Response,
};
use futures::future::BoxFuture;
use gateway_core::engine::{
    CommitRequirement, EngineError,
    execution::{ClientTransport, ExecutionSession, PreparedRootExecution, StartedExecution},
    middleware::{
        MiddlewareBody, MiddlewareError, MiddlewareFrame, MiddlewareFraming, MiddlewareHeader,
        MiddlewareResponse,
    },
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::{ProviderEvent, ProviderResponseHeader};
use gateway_core::lifecycle::{CancellationToken, ConnectionGuard};
use gateway_protocol::openai::sse::response_failed_sse_event_with_id;
use serde_json::Value;
use tokio::time::Instant;

use crate::middleware::headers::encode_headers;
use crate::openai::error::{
    engine_error_response, gateway_error_from_engine, protocol_error_response,
};
use crate::openai::middleware::{
    ExpectedBody, PendingExecution, RequestInput, error_response, finalize_session,
    into_http_response, invoke_request, pending_execution_response, request_parts,
};
use crate::openai::service::OpenAiService;

use super::{
    DecodedGenerationRequest, GenerationProtocol,
    response::GenerationEncoder,
    validation::{GenerationDeliveryValidator, GenerationValidationFacts},
};

const AUTHORIZATION_RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// 管理页面模型桥在等待首帧和交付响应期间复核不可变插件目标
pub(crate) trait ResponseAuthorization: Send + Sync {
    fn authorize(&self) -> BoxFuture<'_, Result<(), GatewayError>>;
}

/// 各协议入口共用的 HTTP 交付上下文
pub(crate) struct GenerationHttpRequest {
    pub peer_address: Option<SocketAddr>,
    pub ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    pub headers: HeaderMap,
    pub decoded: DecodedGenerationRequest,
    pub middleware_body: Bytes,
}

/// 下游协议只投影出口字节 执行会话和计量仍归原 owner
pub(crate) struct HttpResponseAdapter {
    pub endpoint: &'static str,
    pub wrap: Box<dyn FnOnce(MiddlewareResponse) -> MiddlewareResponse + Send>,
    pub stream_error: fn(&Value) -> Bytes,
}

pub(crate) async fn execute_prepared_generation(
    service: OpenAiService,
    prepared: PreparedRootExecution,
    request: GenerationHttpRequest,
    authorization: Option<Arc<dyn ResponseAuthorization>>,
    adapter: Option<HttpResponseAdapter>,
) -> Response {
    let GenerationHttpRequest {
        peer_address,
        ingress_id,
        headers,
        decoded,
        middleware_body,
    } = request;
    let (client_ip, user_agent) = request_client_context(&headers, peer_address);
    let protocol = GenerationProtocol::from_request(&decoded);
    let stream = decoded.metadata().stream();
    let transport = if stream {
        ClientTransport::HttpSse
    } else {
        ClientTransport::HttpJson
    };
    let model_hint = Some(decoded.metadata().requested_model().to_owned());
    // 终端会重新校验中间件改写后的正文，初步解码不必保留到响应交付
    drop(decoded);
    let execution = service.execution();
    let request_id = prepared.request_id().clone();
    let cancellation = prepared.cancellation();
    let execution_entered = Arc::new(AtomicBool::new(false));
    let terminal_entered = Arc::clone(&execution_entered);
    let started_request_id = Arc::new(OnceLock::new());
    let terminal_request_id = Arc::clone(&started_request_id);
    let (admitted, keepalive) = tokio::sync::oneshot::channel();
    let mut middleware_headers = headers.clone();
    // 中间件正文已经由入口按上限解压；传输编码和旧长度不能与改写后的正文混用
    middleware_headers.remove(CONTENT_ENCODING);
    middleware_headers.remove(CONTENT_LENGTH);
    let endpoint = adapter
        .as_ref()
        .map_or(protocol.endpoint(), |adapter| adapter.endpoint);
    let stream_error = adapter
        .as_ref()
        .map(|adapter| adapter.stream_error)
        .or_else(|| {
            (protocol != GenerationProtocol::Responses)
                .then_some(crate::openai::messages::stream_error as fn(&Value) -> Bytes)
        });
    let input = RequestInput {
        endpoint: endpoint.to_owned(),
        protocol: protocol.wire().to_owned(),
        operation: Some(protocol.operation()),
        transport,
        model_hint,
        headers: encode_headers(&middleware_headers),
        body: middleware_body,
    };
    let terminal_service = service.clone();
    let validation = GenerationValidationFacts::new(protocol);
    let terminal_validation = validation.clone();
    let middleware = invoke_request(
        execution,
        prepared,
        input,
        Box::new(move |prepared, request| {
            Box::pin(async move {
                let (wire_protocol, request_headers, request_body) =
                    request_parts(request.clone())?;
                if wire_protocol != protocol.wire() {
                    return Err(MiddlewareError::Rejected);
                }
                let decoded = protocol
                    .decode(
                        &request_body,
                        &request_headers,
                        prepared
                            .client()
                            .snapshot()
                            .responses_max_decompressed_body_bytes(),
                    )
                    .map_err(|_| MiddlewareError::Rejected)?
                    .with_client_context(client_ip, user_agent)
                    .with_middleware_capabilities(&request)?;
                if decoded.metadata().stream() != stream {
                    return Err(MiddlewareError::Rejected);
                }
                let connection_guard = if stream {
                    Some(
                        terminal_service
                            .try_register_connection()
                            .map_err(|_| MiddlewareError::Fault)?,
                    )
                } else {
                    None
                };
                terminal_entered.store(true, Ordering::Relaxed);
                let started = terminal_service
                    .start_prepared_generation(prepared, decoded, transport, endpoint)
                    .await
                    .map_err(MiddlewareError::Gateway)?;
                let _ = terminal_request_id.set(started.request_id.clone());
                started.session.trace().headers(
                    "client.request",
                    serde_json::json!({
                        "ingressRequestId": ingress_id.as_ref().and_then(|Extension(id)| id.header_value().to_str().ok()),
                    }),
                    request_headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_bytes())),
                );
                started
                    .session
                    .trace()
                    .capture("client.request.body", &request_body);
                let _ = admitted.send(started.session.response_commit());
                execution_middleware_response(
                    started,
                    connection_guard,
                    terminal_validation,
                    protocol,
                )
                .await
            })
        }),
    );
    let response = async move {
        let middleware = if let Some(authorization) = authorization.as_ref() {
            match await_with_authorization(middleware, authorization, &cancellation).await {
                Ok(result) => result,
                Err(error) => Err(MiddlewareError::Gateway(error)),
            }
        } else {
            middleware.await
        };
        let expected = if stream {
            ExpectedBody::Sse
        } else {
            ExpectedBody::SingleJson
        };
        match middleware {
            Ok(response) => {
                let response = if let Some(authorization) = authorization {
                    authorized_middleware_response(response, authorization, cancellation)
                } else {
                    response
                };
                let response =
                    validated_middleware_response(response, validation, stream, protocol);
                let response = match adapter {
                    Some(adapter) => (adapter.wrap)(response),
                    None => response,
                };
                into_http_response(response, expected, protocol.wire()).await
            }
            Err(error) => error_response(error),
        }
    };
    let response = if stream {
        crate::openai::responses::keepalive::response_with_encoder(
            response,
            keepalive,
            stream_error,
            protocol == GenerationProtocol::Responses,
        )
        .await
    } else {
        response.await
    };
    let response = if protocol == GenerationProtocol::Responses {
        response
    } else {
        crate::openai::messages::project_error_response(response).await
    };
    // 预留 ID 没有对应执行记录，中间件提前返回时保留 HTTP 入口关联 ID
    if execution_entered.load(Ordering::Relaxed) {
        crate::openai::with_model_request_id(
            response,
            started_request_id.get().unwrap_or(&request_id),
        )
    } else {
        response
    }
}

async fn await_with_authorization<T>(
    future: impl Future<Output = T>,
    authorization: &Arc<dyn ResponseAuthorization>,
    cancellation: &CancellationToken,
) -> Result<T, GatewayError> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Ok(output),
            () = tokio::time::sleep(AUTHORIZATION_RECHECK_INTERVAL) => {
                if let Err(error) = authorization.authorize().await {
                    cancellation.cancel();
                    return Err(error);
                }
            }
        }
    }
}

fn authorized_middleware_response(
    response: MiddlewareResponse,
    authorization: Arc<dyn ResponseAuthorization>,
    cancellation: CancellationToken,
) -> MiddlewareResponse {
    let (protocol, status, headers, body, envelope) = response.into_parts();
    let mut response = MiddlewareResponse::new(
        protocol,
        status,
        headers,
        Box::new(AuthorizedResponseBody {
            inner: Some(body),
            authorization,
            cancellation,
            last_authorized_at: Instant::now(),
        }),
    );
    if let Some(envelope) = envelope {
        response = response.with_envelope(envelope);
    }
    response
}

struct AuthorizedResponseBody {
    inner: Option<Box<dyn MiddlewareBody>>,
    authorization: Arc<dyn ResponseAuthorization>,
    cancellation: CancellationToken,
    last_authorized_at: Instant,
}

impl AuthorizedResponseBody {
    async fn revoke(&mut self, error: GatewayError) -> MiddlewareError {
        self.cancellation.cancel();
        if let Some(inner) = self.inner.take() {
            inner.close().await;
        }
        MiddlewareError::Gateway(error)
    }
}

impl MiddlewareBody for AuthorizedResponseBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            if self.last_authorized_at.elapsed() >= AUTHORIZATION_RECHECK_INTERVAL {
                if let Err(error) = self.authorization.authorize().await {
                    return Err(self.revoke(error).await);
                }
                self.last_authorized_at = Instant::now();
            }
            let error = {
                let Some(inner) = self.inner.as_mut() else {
                    return Ok(None);
                };
                // 复核身份只暂停同一次读取，不能丢弃已经消费上游事件的 future
                let frame = inner.next_frame();
                tokio::pin!(frame);
                loop {
                    let elapsed = self.last_authorized_at.elapsed();
                    if elapsed >= AUTHORIZATION_RECHECK_INTERVAL {
                        if let Err(error) = self.authorization.authorize().await {
                            break error;
                        }
                        self.last_authorized_at = Instant::now();
                        continue;
                    }
                    let wait = AUTHORIZATION_RECHECK_INTERVAL - elapsed;
                    tokio::select! {
                        result = &mut frame => return result,
                        () = tokio::time::sleep(wait) => {}
                    }
                }
            };
            Err(self.revoke(error).await)
        })
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        let Some(inner) = self.inner.as_mut() else {
            return Box::pin(async { Err(MiddlewareError::InvalidState) });
        };
        inner.commit_downstream(client_status_code)
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        let Some(inner) = self.inner.as_mut() else {
            return Box::pin(async { Ok(()) });
        };
        inner.record_client_status(client_status_code)
    }

    fn is_finalized(&self) -> bool {
        self.inner.as_ref().is_none_or(|inner| inner.is_finalized())
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        let inner = self.inner.take();
        Box::pin(async move {
            if let Some(inner) = inner {
                inner.close().await;
            }
        })
    }
}

async fn execution_middleware_response(
    started: StartedExecution,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
    validation: GenerationValidationFacts,
    protocol: GenerationProtocol,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let StartedExecution {
        stream, session, ..
    } = started;
    if stream {
        streaming_execution_middleware_response(session, connection_guard, validation, protocol)
            .await
    } else {
        drop(connection_guard);
        buffered_execution_middleware_response(session, validation, protocol).await
    }
}

async fn buffered_execution_middleware_response(
    session: Box<dyn ExecutionSession>,
    validation: GenerationValidationFacts,
    protocol: GenerationProtocol,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let mut execution = PendingExecution::new(session);
    let collected = execution
        .session_mut()
        .ok_or(MiddlewareError::InvalidState)?
        .collect_uncommitted()
        .await;
    let events = match collected {
        Ok(events) => events,
        Err(error) => {
            let headers = execution
                .session_mut()
                .ok_or(MiddlewareError::InvalidState)?
                .response_headers()
                .to_vec();
            return guarded_engine_error_response(execution, &error, &headers, protocol).await;
        }
    };
    let transformed = events.iter().any(ProviderEvent::middleware_transformed);
    let encoded = match protocol.collect(&events) {
        Ok(encoded) => Bytes::from(encoded),
        Err(error) => {
            let response =
                protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error.protocol_body());
            return guarded_http_response(execution, response, protocol).await;
        }
    };
    validation.observe_buffered(&events, &encoded);
    let session = execution
        .session_mut()
        .ok_or(MiddlewareError::InvalidState)?;
    session.trace().record(
        "downstream.encoded",
        serde_json::json!({"transport": "http_json", "bytes": encoded.len()}),
    );
    session.trace().dump("downstream.body", &encoded);
    let headers = response_headers(session.response_headers(), false);
    Ok(pending_execution_response(
        protocol.wire().to_owned(),
        StatusCode::OK.as_u16(),
        headers,
        MiddlewareFrame::new(encoded, MiddlewareFraming::JsonDocument, true)
            .with_transformed(transformed),
        execution,
    ))
}

async fn streaming_execution_middleware_response(
    session: Box<dyn ExecutionSession>,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
    validation: GenerationValidationFacts,
    protocol: GenerationProtocol,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let mut body = StreamingExecutionBody::new(session, connection_guard, validation, protocol);
    if let Err(error) = body.prime().await {
        let headers = body.response_headers().to_vec();
        let execution = body.take_pending_execution()?;
        return match error {
            MiddlewareError::Engine(error) => {
                guarded_engine_error_response(execution, &error, &headers, protocol).await
            }
            error => guarded_http_response(execution, error_response(error), protocol).await,
        };
    }
    let headers = response_headers(body.response_headers(), true);
    Ok(MiddlewareResponse::new(
        protocol.wire().to_owned(),
        StatusCode::OK.as_u16(),
        headers,
        Box::new(body),
    ))
}

/// 直接驱动测试 session 的非流式交付；生产入口通过同一 MiddlewareResponse 路径
pub async fn collect_execution_response(session: Box<dyn ExecutionSession>) -> Response {
    match buffered_execution_middleware_response(
        session,
        GenerationValidationFacts::new(GenerationProtocol::Responses),
        GenerationProtocol::Responses,
    )
    .await
    {
        Ok(response) => into_http_response(response, ExpectedBody::SingleJson, "openai").await,
        Err(error) => error_response(error),
    }
}

/// 直接驱动测试 session 的 SSE 交付；生产入口通过同一 MiddlewareResponse 路径
pub async fn stream_execution_response(
    session: Box<dyn ExecutionSession>,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
) -> Response {
    let (admitted, keepalive) = tokio::sync::oneshot::channel();
    let _ = admitted.send(session.response_commit());
    crate::openai::responses::keepalive::response(
        async move {
            match streaming_execution_middleware_response(
                session,
                connection_guard,
                GenerationValidationFacts::new(GenerationProtocol::Responses),
                GenerationProtocol::Responses,
            )
            .await
            {
                Ok(response) => into_http_response(response, ExpectedBody::Sse, "openai").await,
                Err(error) => error_response(error),
            }
        },
        keepalive,
    )
    .await
}

fn validated_middleware_response(
    response: MiddlewareResponse,
    facts: GenerationValidationFacts,
    streaming: bool,
    protocol: GenerationProtocol,
) -> MiddlewareResponse {
    if !(200..300).contains(&response.status_code()) {
        // 非成功响应沿用通用 SinglePayload 边界，不能套用成功 Responses 终态状态机
        return response;
    }
    let (wire_protocol, status, headers, body, envelope) = response.into_parts();
    let mut response = MiddlewareResponse::new(
        wire_protocol,
        status,
        headers,
        Box::new(ValidatedGenerationBody {
            inner: Some(body),
            facts,
            streaming,
            validator: GenerationDeliveryValidator::new(protocol),
            protocol,
            pending: VecDeque::new(),
            committed: false,
        }),
    );
    if let Some(envelope) = envelope {
        response = response.with_envelope(envelope);
    }
    response
}

struct ValidatedGenerationBody {
    inner: Option<Box<dyn MiddlewareBody>>,
    facts: GenerationValidationFacts,
    streaming: bool,
    validator: GenerationDeliveryValidator,
    protocol: GenerationProtocol,
    pending: VecDeque<MiddlewareFrame>,
    committed: bool,
}

impl ValidatedGenerationBody {
    async fn fail_stream(&mut self) -> Option<MiddlewareFrame> {
        if let Some(inner) = self.inner.take() {
            inner.close().await;
        }
        let response_id = self
            .validator
            .response_id()
            .map(str::to_owned)
            .or_else(|| self.facts.response_id());
        self.pending.push_back(MiddlewareFrame::new(
            if self.protocol == GenerationProtocol::Responses {
                Bytes::from(response_failed_sse_event_with_id(response_id.as_deref(), "server_error", "middleware_failed", "Response middleware returned an invalid stream."))
            } else {
                crate::openai::messages::stream_error(&serde_json::json!({"error":{"message":"Response middleware returned an invalid stream."}}))
            },
            MiddlewareFraming::SseEvent,
            false,
        ));
        self.pending.push_back(done_frame(self.protocol));
        self.pending.pop_front()
    }
}

impl MiddlewareBody for ValidatedGenerationBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }
            let Some(inner) = self.inner.as_mut() else {
                return Ok(None);
            };
            let frame = match inner.next_frame().await {
                Ok(frame) => frame,
                Err(error) if self.streaming && self.committed => {
                    let _ = error;
                    return Ok(self.fail_stream().await);
                }
                Err(error) => return Err(error),
            };
            let Some(frame) = frame else {
                if self.streaming && self.validator.finish_delivery().is_err() {
                    if self.committed {
                        return Ok(self.fail_stream().await);
                    }
                    return Err(MiddlewareError::InvalidState);
                }
                return Ok(None);
            };
            let valid = if self.streaming {
                self.validator
                    .validate_frame(frame.bytes(), frame.transformed(), &self.facts)
            } else if frame.transformed() {
                self.facts.validate_buffered(frame.bytes())
            } else {
                Ok(())
            };
            if valid.is_err() {
                if self.streaming && self.committed {
                    return Ok(self.fail_stream().await);
                }
                return Err(MiddlewareError::InvalidState);
            }
            Ok(Some(frame))
        })
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            let Some(inner) = self.inner.as_mut() else {
                return Err(MiddlewareError::InvalidState);
            };
            inner.commit_downstream(client_status_code).await?;
            self.committed = true;
            Ok(())
        })
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            match self.inner.as_mut() {
                Some(inner) => inner.record_client_status(client_status_code).await,
                None => Ok(()),
            }
        })
    }

    fn is_finalized(&self) -> bool {
        self.inner.as_ref().is_none_or(|inner| inner.is_finalized())
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        let inner = self.inner.take();
        Box::pin(async move {
            if let Some(inner) = inner {
                inner.close().await;
            }
        })
    }
}

async fn guarded_engine_error_response(
    execution: PendingExecution,
    error: &EngineError,
    response_headers: &[ProviderResponseHeader],
    protocol: GenerationProtocol,
) -> Result<MiddlewareResponse, MiddlewareError> {
    guarded_http_response(
        execution,
        engine_error_response_with_headers(error, response_headers),
        protocol,
    )
    .await
}

async fn guarded_http_response(
    execution: PendingExecution,
    response: Response,
    protocol: GenerationProtocol,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let response = if protocol == GenerationProtocol::Responses {
        response
    } else {
        crate::openai::messages::project_error_response(response).await
    };
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| MiddlewareError::Fault)?;
    Ok(pending_execution_response(
        protocol.wire().to_owned(),
        parts.status.as_u16(),
        encode_headers(&parts.headers),
        MiddlewareFrame::new(bytes, MiddlewareFraming::RawBytes, true),
        execution,
    ))
}

fn engine_error_response_with_headers(
    error: &EngineError,
    response_headers: &[ProviderResponseHeader],
) -> Response {
    let response = engine_error_response(error);
    let failure_request_ids = ["x-request-id", "x-oai-request-id"]
        .into_iter()
        .flat_map(|name| {
            response
                .headers()
                .get_all(name)
                .iter()
                .cloned()
                .map(move |value| (name, value))
        })
        .collect::<Vec<_>>();
    // 失败后仍须交付已采集的 turn state；只隔离 opening 身份，不丢弃会话状态
    let mut response = apply_provider_response_headers(response, response_headers);
    response.headers_mut().remove("x-request-id");
    response.headers_mut().remove("x-oai-request-id");
    for (name, value) in failure_request_ids {
        response.headers_mut().append(name, value);
    }
    response
}

fn apply_provider_response_headers(
    mut response: Response,
    response_headers: &[ProviderResponseHeader],
) -> Response {
    let connection_options =
        crate::openai::responses::response_connection_options(response_headers);
    for header in response_headers {
        if !crate::openai::responses::response_header_is_forwardable(
            header.name(),
            &connection_options,
        ) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.name().as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_bytes(header.value()) else {
            continue;
        };
        response.headers_mut().append(name, value);
    }
    response
}

fn response_headers(
    provider_headers: &[ProviderResponseHeader],
    streaming: bool,
) -> Vec<MiddlewareHeader> {
    let connection_options =
        crate::openai::responses::response_connection_options(provider_headers);
    let mut headers = Vec::new();
    headers.push(MiddlewareHeader::new(
        CONTENT_TYPE.as_str(),
        Bytes::from_static(if streaming {
            b"text/event-stream"
        } else {
            b"application/json"
        }),
    ));
    for header in provider_headers {
        if !crate::openai::responses::response_header_is_forwardable(
            header.name(),
            &connection_options,
        ) || header.name().eq_ignore_ascii_case(CONTENT_TYPE.as_str())
            || HeaderName::from_bytes(header.name().as_bytes()).is_err()
            || HeaderValue::from_bytes(header.value()).is_err()
        {
            continue;
        }
        headers.push(MiddlewareHeader::new(
            header.name(),
            Bytes::copy_from_slice(header.value()),
        ));
    }
    headers
}

struct StreamingExecutionBody {
    session: Option<Box<dyn ExecutionSession>>,
    encoder: GenerationEncoder,
    protocol: GenerationProtocol,
    validation: GenerationValidationFacts,
    pending: VecDeque<MiddlewareFrame>,
    awaiting_terminal_eof: bool,
    output_finished: bool,
    batch_pending: bool,
    transformed_pending: bool,
    committed: bool,
    _connection_guard: Option<Box<dyn ConnectionGuard>>,
}

impl StreamingExecutionBody {
    fn new(
        session: Box<dyn ExecutionSession>,
        connection_guard: Option<Box<dyn ConnectionGuard>>,
        validation: GenerationValidationFacts,
        protocol: GenerationProtocol,
    ) -> Self {
        Self {
            session: Some(session),
            encoder: GenerationEncoder::new(protocol),
            protocol,
            validation,
            pending: VecDeque::new(),
            awaiting_terminal_eof: false,
            output_finished: false,
            batch_pending: false,
            transformed_pending: false,
            committed: false,
            _connection_guard: connection_guard,
        }
    }

    async fn prime(&mut self) -> Result<(), MiddlewareError> {
        self.fill_pending().await?;
        if self.pending.is_empty() {
            return Err(MiddlewareError::InvalidState);
        }
        Ok(())
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        self.session
            .as_ref()
            .map_or(&[], |session| session.response_headers())
    }

    async fn fill_pending(&mut self) -> Result<(), MiddlewareError> {
        while self.pending.is_empty() && !self.output_finished {
            if self.awaiting_terminal_eof {
                if !self.committed {
                    // 终态 wire 不能在首字节前被中间件整批移除
                    return Err(MiddlewareError::InvalidState);
                }
                self.verify_terminal_eof().await?;
                self.pending.push_back(done_frame(self.protocol));
                self.output_finished = true;
                continue;
            }
            if self.batch_pending {
                if !self.committed {
                    self.session
                        .as_mut()
                        .ok_or(MiddlewareError::InvalidState)?
                        .discard_pending_delivery()
                        .map_err(MiddlewareError::Engine)?;
                }
                self.batch_pending = false;
            }
            let result = self
                .session
                .as_mut()
                .ok_or(MiddlewareError::InvalidState)?
                .next_event()
                .await;
            let coordinated = match result {
                Ok(Some(event)) => event,
                Ok(None) if self.committed && self.is_finalized() => {
                    self.pending.push_back(done_frame(self.protocol));
                    self.output_finished = true;
                    continue;
                }
                Ok(None) => return Err(invalid_execution_stream()),
                Err(error) if self.committed => {
                    self.push_stream_error(gateway_error_from_engine(&error));
                    continue;
                }
                Err(error) => return Err(MiddlewareError::Engine(error)),
            };
            let expected = if self.committed {
                CommitRequirement::AlreadyCommitted
            } else {
                CommitRequirement::CommitBeforeDelivery
            };
            if coordinated.commit_requirement() != expected {
                return Err(invalid_execution_stream());
            }
            let mut encoded = VecDeque::new();
            for event in coordinated.into_provider_events() {
                self.validation.observe_event(&event);
                let transformed = self.transformed_pending || event.middleware_transformed();
                let frames = self.encoder.push_sse(&event);
                if frames.is_empty() {
                    self.transformed_pending = transformed;
                    continue;
                }
                encoded.extend(frames.into_iter().map(|bytes| {
                    MiddlewareFrame::new(bytes, MiddlewareFraming::SseEvent, false)
                        .with_transformed(transformed)
                }));
                self.transformed_pending = false;
            }
            self.awaiting_terminal_eof =
                self.encoder.is_completed() || self.encoder.has_wire_failure();
            self.batch_pending = !self.committed;
            if encoded.is_empty() {
                if !self.committed || self.awaiting_terminal_eof {
                    return Err(invalid_execution_stream());
                }
                continue;
            }
            // 已提交的流先确认 Core 终结，避免后续 EOF 验证推翻已交付的成功终态
            if self.awaiting_terminal_eof && self.committed {
                self.verify_terminal_eof().await?;
                encoded.push_back(done_frame(self.protocol));
                self.output_finished = true;
            }
            self.pending = encoded;
        }
        Ok(())
    }

    async fn verify_terminal_eof(&mut self) -> Result<(), MiddlewareError> {
        let result = self
            .session
            .as_mut()
            .ok_or(MiddlewareError::InvalidState)?
            .next_event()
            .await;
        match result {
            Ok(None) if self.is_finalized() => Ok(()),
            Err(_) if self.encoder.has_wire_failure() && self.is_finalized() => Ok(()),
            Ok(_) => Err(invalid_execution_stream()),
            Err(error) => Err(MiddlewareError::Engine(error)),
        }
    }

    fn push_stream_error(&mut self, error: GatewayError) {
        self.pending.push_back(MiddlewareFrame::new(
            self.encoder.error_frame(&error),
            MiddlewareFraming::SseEvent,
            false,
        ));
        self.pending.push_back(done_frame(self.protocol));
        self.output_finished = true;
    }

    fn take_session_for_close(&mut self) -> Option<Box<dyn ExecutionSession>> {
        self.session.take()
    }

    fn take_pending_execution(&mut self) -> Result<PendingExecution, MiddlewareError> {
        self.take_session_for_close()
            .map(PendingExecution::new)
            .ok_or(MiddlewareError::InvalidState)
    }
}

impl MiddlewareBody for StreamingExecutionBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }
            if self.output_finished {
                return Ok(None);
            }
            if let Err(error) = self.fill_pending().await {
                if !self.committed {
                    return Err(error);
                }
                let error = match error {
                    MiddlewareError::Gateway(error) => error,
                    MiddlewareError::Engine(error) => gateway_error_from_engine(&error),
                    _ => GatewayError::new(
                        GatewayErrorKind::Internal,
                        "Gateway response stream could not be delivered.",
                    ),
                };
                self.push_stream_error(error);
            }
            Ok(self.pending.pop_front())
        })
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            if self.committed {
                return Ok(());
            }
            self.session
                .as_mut()
                .ok_or(MiddlewareError::InvalidState)?
                .commit_downstream(client_status_code)
                .await
                .map_err(MiddlewareError::Engine)?;
            self.committed = true;
            self.batch_pending = false;
            Ok(())
        })
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            let Some(session) = self.session.as_mut() else {
                return Ok(());
            };
            session
                .record_client_status(client_status_code)
                .await
                .map_err(MiddlewareError::Engine)
        })
    }

    fn is_finalized(&self) -> bool {
        self.session
            .as_ref()
            .is_none_or(|session| session.is_finalized())
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        let session = self.take_session_for_close();
        Box::pin(async move {
            if let Some(session) = session {
                finalize_session(session).await;
            }
        })
    }
}

impl Drop for StreamingExecutionBody {
    fn drop(&mut self) {
        let Some(session) = self.take_session_for_close() else {
            return;
        };
        session.cancel();
        let finalize = session.detach_finalize();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn(finalize));
        }
    }
}

fn done_frame(protocol: GenerationProtocol) -> MiddlewareFrame {
    MiddlewareFrame::new(protocol.terminal_bytes(), MiddlewareFraming::SseEvent, true)
}

fn invalid_execution_stream() -> MiddlewareError {
    MiddlewareError::Gateway(GatewayError::new(
        GatewayErrorKind::Internal,
        "Gateway response stream violated its execution lifecycle.",
    ))
}
