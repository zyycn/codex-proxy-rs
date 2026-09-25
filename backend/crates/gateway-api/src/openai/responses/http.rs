//! OpenAI Responses HTTP 与 SSE adapter。

use std::collections::VecDeque;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode,
        header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
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
use gateway_core::operation::OperationKind;
use gateway_protocol::openai::sse::{DONE_SSE_FRAME, response_failed_sse_event_with_id};
use tokio::time::Instant;

use crate::ApiState;
use crate::openai::middleware::{
    ExpectedBody, HttpMiddlewareInput, PendingExecution, error_response, finalize_session,
    into_http_response, invoke_http_middleware, pending_execution_response, request_headers,
    request_parts,
};
use crate::openai::service::OpenAiService;
use crate::openai::{
    auth::{authenticate_client, client_access_error_response},
    error::{
        engine_error_response, gateway_error_contract, gateway_error_from_engine,
        gateway_error_response, protocol_error_response,
    },
};

use super::{
    OpenAiResponsesEncoder, ResponseEncodeError,
    request::{DecodedResponsesRequest, decode_request_with_body, decode_request_with_headers},
    validation::{ResponseValidationFacts, ResponsesDeliveryValidator, validate_buffered_response},
};

const OPENAI_PROTOCOL: &str = "openai";
const AUTHORIZATION_RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// 管理页面模型桥在等待首帧和交付响应期间复核不可变插件目标。
pub(crate) trait ResponseAuthorization: Send + Sync {
    fn authorize(&self) -> BoxFuture<'_, Result<(), GatewayError>>;
}

/// `POST /v1/responses`。
pub(crate) async fn responses(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai().clone();
    let client = match authenticate_client(&service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let maximum_body_bytes = client.snapshot().responses_max_decompressed_body_bytes();
    let (decoded, middleware_body) =
        match decode_request_with_body(&body, &headers, maximum_body_bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                return super::super::error::protocol_error_response(
                    StatusCode::BAD_REQUEST,
                    error.protocol_body(),
                );
            }
        };
    let execution = service.execution();
    let prepared = match execution.prepare_execution(client).await {
        Ok(prepared) => prepared,
        Err(error) => return gateway_error_response(&error),
    };
    execute_prepared_responses(
        service,
        prepared,
        ResponsesHttpRequest {
            peer_address: connect_info.map(|Extension(ConnectInfo(address))| address),
            ingress_id,
            headers,
            decoded,
            middleware_body,
            maximum_body_bytes,
        },
        None,
    )
    .await
}

/// 两个 Responses 入口共享解码后的 HTTP 交付上下文，不携带认证策略。
pub(crate) struct ResponsesHttpRequest {
    pub peer_address: Option<SocketAddr>,
    pub ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    pub headers: HeaderMap,
    pub decoded: DecodedResponsesRequest,
    pub middleware_body: Bytes,
    pub maximum_body_bytes: usize,
}

pub(crate) async fn execute_prepared_responses(
    service: OpenAiService,
    prepared: PreparedRootExecution,
    request: ResponsesHttpRequest,
    authorization: Option<Arc<dyn ResponseAuthorization>>,
) -> Response {
    let ResponsesHttpRequest {
        peer_address,
        ingress_id,
        headers,
        decoded,
        middleware_body,
        maximum_body_bytes,
    } = request;
    let (client_ip, user_agent) = request_client_context(&headers, peer_address);
    let stream = decoded.metadata().stream();
    let transport = if stream {
        ClientTransport::HttpSse
    } else {
        ClientTransport::HttpJson
    };
    let model_hint = Some(decoded.metadata().requested_model().to_owned());
    let execution = service.execution();
    let request_id = prepared.request_id().clone();
    let cancellation = prepared.cancellation();
    let started_request_id = Arc::new(OnceLock::new());
    let terminal_request_id = Arc::clone(&started_request_id);
    let mut middleware_headers = headers.clone();
    // 中间件正文已经由入口按上限解压；传输编码和旧长度不能与改写后的正文混用。
    middleware_headers.remove(CONTENT_ENCODING);
    middleware_headers.remove(CONTENT_LENGTH);
    let input = HttpMiddlewareInput {
        endpoint: crate::openai::router::RESPONSES_PATH.to_owned(),
        protocol: OPENAI_PROTOCOL.to_owned(),
        operation: Some(OperationKind::Generate),
        transport,
        model_hint,
        headers: request_headers(&middleware_headers),
        body: middleware_body,
    };
    let terminal_service = service.clone();
    let validation = ResponseValidationFacts::default();
    let terminal_validation = validation.clone();
    let middleware = invoke_http_middleware(
        execution,
        prepared,
        input,
        Box::new(move |prepared, request| {
            Box::pin(async move {
                let (protocol, request_headers, request_body) = request_parts(request.clone())?;
                if protocol != OPENAI_PROTOCOL {
                    return Err(MiddlewareError::Rejected);
                }
                let decoded = decode_request_with_headers(
                    &request_body,
                    &request_headers,
                    maximum_body_bytes,
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
                let started = terminal_service
                    .start_prepared_response(
                        prepared,
                        decoded,
                        transport,
                        crate::openai::router::RESPONSES_PATH,
                    )
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
                execution_middleware_response(started, connection_guard, terminal_validation).await
            })
        }),
    );
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
    let response = match middleware {
        Ok(response) => {
            let response = if let Some(authorization) = authorization {
                authorized_middleware_response(response, authorization, cancellation)
            } else {
                response
            };
            into_http_response(
                validated_middleware_response(response, validation, stream),
                expected,
            )
            .await
        }
        Err(error) => error_response(error),
    };
    super::super::with_model_request_id(response, started_request_id.get().unwrap_or(&request_id))
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
            loop {
                let elapsed = self.last_authorized_at.elapsed();
                if elapsed >= AUTHORIZATION_RECHECK_INTERVAL {
                    if let Err(error) = self.authorization.authorize().await {
                        return Err(self.revoke(error).await);
                    }
                    self.last_authorized_at = Instant::now();
                    continue;
                }
                let Some(inner) = self.inner.as_mut() else {
                    return Ok(None);
                };
                let wait = AUTHORIZATION_RECHECK_INTERVAL - elapsed;
                tokio::select! {
                    frame = inner.next_frame() => return frame,
                    () = tokio::time::sleep(wait) => {}
                }
            }
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

/// 从 socket 与标准转发头提取旧 Usage 页面使用的诊断事实。
pub(crate) fn request_client_context(
    headers: &HeaderMap,
    peer_address: Option<SocketAddr>,
) -> (Option<IpAddr>, Option<String>) {
    let client_ip = ["cf-connecting-ip", "x-real-ip"]
        .into_iter()
        .find_map(|name| header_ip(headers, name))
        .or_else(|| forwarded_client_ip(headers))
        .or_else(|| peer_address.map(|address| address.ip()));
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    (client_ip, user_agent)
}

fn header_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| value.parse().ok())
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let addresses = headers
        .get("x-forwarded-for")?
        .to_str()
        .ok()?
        .split(',')
        .filter_map(|value| value.trim().parse::<IpAddr>().ok())
        .collect::<Vec<_>>();
    addresses
        .iter()
        .copied()
        .find(|address| !is_private_or_loopback(*address))
        .or_else(|| addresses.first().copied())
}

const fn is_private_or_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_loopback(),
        IpAddr::V6(address) => address.is_unique_local() || address.is_loopback(),
    }
}

async fn execution_middleware_response(
    started: StartedExecution,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
    validation: ResponseValidationFacts,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let StartedExecution {
        stream, session, ..
    } = started;
    if stream {
        streaming_execution_middleware_response(session, connection_guard, validation).await
    } else {
        drop(connection_guard);
        buffered_execution_middleware_response(session, validation).await
    }
}

async fn buffered_execution_middleware_response(
    session: Box<dyn ExecutionSession>,
    validation: ResponseValidationFacts,
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
            return guarded_engine_error_response(execution, &error, &headers).await;
        }
    };
    let transformed = events.iter().any(ProviderEvent::middleware_transformed);
    let encoded = match encode_collected_events(&events) {
        Ok(encoded) => Bytes::from(encoded),
        Err(error) => {
            let response =
                protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error.protocol_body());
            return guarded_http_response(execution, response).await;
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
        OPENAI_PROTOCOL.to_owned(),
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
    validation: ResponseValidationFacts,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let mut body = StreamingExecutionBody::new(session, connection_guard, validation);
    if let Err(error) = body.prime().await {
        let headers = body.response_headers().to_vec();
        let execution = body.take_pending_execution()?;
        return match error {
            MiddlewareError::Engine(error) => {
                guarded_engine_error_response(execution, &error, &headers).await
            }
            error => guarded_http_response(execution, error_response(error)).await,
        };
    }
    let headers = response_headers(body.response_headers(), true);
    Ok(MiddlewareResponse::new(
        OPENAI_PROTOCOL.to_owned(),
        StatusCode::OK.as_u16(),
        headers,
        Box::new(body),
    ))
}

/// 直接驱动测试 session 的非流式交付；生产入口通过同一 MiddlewareResponse 路径。
pub async fn collect_execution_response(session: Box<dyn ExecutionSession>) -> Response {
    match buffered_execution_middleware_response(session, ResponseValidationFacts::default()).await
    {
        Ok(response) => into_http_response(response, ExpectedBody::SingleJson).await,
        Err(error) => error_response(error),
    }
}

/// 直接驱动测试 session 的 SSE 交付；生产入口通过同一 MiddlewareResponse 路径。
pub async fn stream_execution_response(
    session: Box<dyn ExecutionSession>,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
) -> Response {
    match streaming_execution_middleware_response(
        session,
        connection_guard,
        ResponseValidationFacts::default(),
    )
    .await
    {
        Ok(response) => into_http_response(response, ExpectedBody::Sse).await,
        Err(error) => error_response(error),
    }
}

fn validated_middleware_response(
    response: MiddlewareResponse,
    facts: ResponseValidationFacts,
    streaming: bool,
) -> MiddlewareResponse {
    if !(200..300).contains(&response.status_code()) {
        // 非成功响应沿用通用 SinglePayload 边界，不能套用成功 Responses 终态状态机。
        return response;
    }
    let (protocol, status, headers, body, envelope) = response.into_parts();
    let mut response = MiddlewareResponse::new(
        protocol,
        status,
        headers,
        Box::new(ValidatedResponsesBody {
            inner: Some(body),
            facts,
            streaming,
            validator: ResponsesDeliveryValidator::default(),
            pending: VecDeque::new(),
            committed: false,
        }),
    );
    if let Some(envelope) = envelope {
        response = response.with_envelope(envelope);
    }
    response
}

struct ValidatedResponsesBody {
    inner: Option<Box<dyn MiddlewareBody>>,
    facts: ResponseValidationFacts,
    streaming: bool,
    validator: ResponsesDeliveryValidator,
    pending: VecDeque<MiddlewareFrame>,
    committed: bool,
}

impl ValidatedResponsesBody {
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
            Bytes::from(response_failed_sse_event_with_id(
                response_id.as_deref(),
                "server_error",
                "middleware_failed",
                "Response middleware returned an invalid stream.",
            )),
            MiddlewareFraming::SseEvent,
            false,
        ));
        self.pending.push_back(done_frame());
        self.pending.pop_front()
    }
}

impl MiddlewareBody for ValidatedResponsesBody {
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
                validate_buffered_response(&self.facts, frame.bytes())
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

fn encode_collected_events(events: &[ProviderEvent]) -> Result<Vec<u8>, ResponseEncodeError> {
    let response = super::response::collect_response(events)?;
    serde_json::to_vec(&response).map_err(|_| ResponseEncodeError::Serialization)
}

async fn guarded_engine_error_response(
    execution: PendingExecution,
    error: &EngineError,
    response_headers: &[ProviderResponseHeader],
) -> Result<MiddlewareResponse, MiddlewareError> {
    guarded_http_response(
        execution,
        engine_error_response_with_headers(error, response_headers),
    )
    .await
}

async fn guarded_http_response(
    execution: PendingExecution,
    response: Response,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| MiddlewareError::Fault)?;
    Ok(pending_execution_response(
        OPENAI_PROTOCOL.to_owned(),
        parts.status.as_u16(),
        request_headers(&parts.headers),
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
    // 失败后仍须交付已采集的 turn state；只隔离 opening 身份，不丢弃会话状态。
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
    let connection_options = super::response_connection_options(response_headers);
    for header in response_headers {
        if !super::response_header_is_forwardable(header.name(), &connection_options) {
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
    let connection_options = super::response_connection_options(provider_headers);
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
        if !super::response_header_is_forwardable(header.name(), &connection_options)
            || header.name().eq_ignore_ascii_case(CONTENT_TYPE.as_str())
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
    encoder: OpenAiResponsesEncoder,
    validation: ResponseValidationFacts,
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
        validation: ResponseValidationFacts,
    ) -> Self {
        Self {
            session: Some(session),
            encoder: OpenAiResponsesEncoder::new(),
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
                    // 终态 wire 不能在首字节前被中间件整批移除。
                    return Err(MiddlewareError::InvalidState);
                }
                self.verify_terminal_eof().await?;
                self.pending.push_back(done_frame());
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
                    self.pending.push_back(done_frame());
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
            // 已提交的流在确认 Core 终结后才交付成功终态，不能先输出 completed 再报结算失败。
            if self.awaiting_terminal_eof && self.committed {
                self.verify_terminal_eof().await?;
                encoded.push_back(done_frame());
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
        let (_, default_type, default_code) = gateway_error_contract(error.kind());
        self.pending.push_back(MiddlewareFrame::new(
            Bytes::from(response_failed_sse_event_with_id(
                self.encoder.response_id(),
                error.client_error_type().unwrap_or(default_type),
                super::super::error::client_error_code(
                    error.client_error_code().unwrap_or(default_code),
                ),
                error.client_message(),
            )),
            MiddlewareFraming::SseEvent,
            false,
        ));
        self.pending.push_back(done_frame());
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
                        "gateway response stream could not be delivered",
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

fn done_frame() -> MiddlewareFrame {
    MiddlewareFrame::new(
        Bytes::from_static(DONE_SSE_FRAME.as_bytes()),
        MiddlewareFraming::SseEvent,
        true,
    )
}

fn invalid_execution_stream() -> MiddlewareError {
    MiddlewareError::Gateway(GatewayError::new(
        GatewayErrorKind::Internal,
        "gateway response stream violated its execution lifecycle",
    ))
}
