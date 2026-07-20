//! OpenAI Responses HTTP 与 SSE adapter。

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};

use axum::{
    body::{Body, Bytes},
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, USER_AGENT},
    },
    response::{IntoResponse, Response},
};
use futures::stream;
use gateway_core::engine::CommitRequirement;
use gateway_core::engine::execution::{ClientTransport, ExecutionSession, StartedExecution};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::{ProviderEvent, ProviderResponseHeader};
use gateway_core::lifecycle::ConnectionGuard;
use gateway_protocol::openai::sse::{DONE_SSE_FRAME, response_failed_sse_event};

use crate::ApiState;
use crate::openai::{
    auth::{authenticate_client, authentication_error_response},
    error::{
        gateway_error_contract, gateway_error_from_engine, gateway_error_response,
        protocol_error_response, runtime_unavailable_response,
    },
    service::created_at_unix_seconds,
};

use super::{
    OpenAiResponsesEncoder, ProtocolErrorBody, ResponseEncodeError,
    request::{decode_request_with_headers, decode_review_request_with_headers},
};

/// `POST /v1/responses`。
pub(crate) async fn responses(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(state, connect_info, headers, body, false).await
}

/// `POST /v1/responses/review`。
pub(crate) async fn review_responses(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(state, connect_info, headers, body, true).await
}

async fn handle_responses(
    state: ApiState,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
    review: bool,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let decoded = match if review {
        decode_review_request_with_headers(&body, &headers)
    } else {
        decode_request_with_headers(&body, &headers)
    } {
        Ok(decoded) => decoded,
        Err(error) => {
            return protocol_error_response(StatusCode::BAD_REQUEST, error.protocol_body());
        }
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    let decoded = decoded.with_client_context(client_ip, user_agent);
    let streaming = decoded.metadata().stream();
    let connection_guard = if streaming {
        match service.try_register_connection() {
            Ok(guard) => Some(guard),
            Err(_) => return runtime_unavailable_response().into_response(),
        }
    } else {
        None
    };
    let started = match service
        .start_response(
            client,
            decoded,
            if streaming {
                ClientTransport::HttpSse
            } else {
                ClientTransport::HttpJson
            },
            if review {
                "/v1/responses/review"
            } else {
                "/v1/responses"
            },
        )
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    let StartedExecution {
        created_at,
        stream,
        session,
        ..
    } = started;
    let created_at = match created_at_unix_seconds(created_at) {
        Ok(created_at) => created_at,
        Err(error) => {
            let mut execution = PendingExecution::new(session);
            let response = gateway_error_response(&error);
            return execution.record_response_status(response).await;
        }
    };
    if stream {
        stream_execution_response(session, created_at, connection_guard).await
    } else {
        drop(connection_guard);
        collect_execution_response(session, created_at).await
    }
}

/// 从 socket 与标准转发头提取旧 Usage 页面使用的诊断事实。
pub(super) fn request_client_context(
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

/// 编码完整 canonical event 集合，并在完整 JSON 成功后提交下游。
pub async fn collect_execution_response(
    session: Box<dyn ExecutionSession>,
    created_at: u64,
) -> Response {
    let mut execution = PendingExecution::new(session);
    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let events = match session.collect_uncommitted().await {
        Ok(events) => events,
        Err(error) => {
            let response = gateway_error_response(&gateway_error_from_engine(&error));
            return execution.record_response_status(response).await;
        }
    };

    let encoded = match encode_collected_events(created_at, &events) {
        Ok(encoded) => encoded,
        Err(error) => {
            let response =
                protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error.protocol_body());
            let response = execution.record_response_status(response).await;
            execution.cancel_and_finalize().await;
            return response;
        }
    };
    let response_headers = session.response_headers().to_vec();
    let response = json_body_response(encoded, &response_headers);

    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    if let Err(error) = session
        .commit_downstream(Some(StatusCode::OK.as_u16()))
        .await
    {
        execution.cancel_and_finalize().await;
        let response = gateway_error_response(&gateway_error_from_engine(&error));
        return execution.record_response_status(response).await;
    }
    if !session.is_finalized() {
        execution.cancel_and_finalize().await;
        return internal_gateway_response("gateway response was not finalized after commit");
    }
    execution.disarm();
    response
}

#[derive(Debug)]
enum BufferedResponseEncodeError {
    Canonical(ResponseEncodeError),
    Json,
}

impl BufferedResponseEncodeError {
    fn protocol_body(&self) -> ProtocolErrorBody {
        match self {
            Self::Canonical(error) => error.protocol_body(),
            Self::Json => ResponseEncodeError::UnsupportedEvent.protocol_body(),
        }
    }
}

fn encode_collected_events(
    created_at: u64,
    events: &[ProviderEvent],
) -> Result<Vec<u8>, BufferedResponseEncodeError> {
    let mut encoder = OpenAiResponsesEncoder::new(created_at);
    for event in events {
        encoder
            .push_sse(event)
            .map_err(BufferedResponseEncodeError::Canonical)?;
    }
    let response = encoder
        .finish()
        .map_err(BufferedResponseEncodeError::Canonical)?;
    serde_json::to_vec(&response).map_err(|_| BufferedResponseEncodeError::Json)
}

fn json_body_response(encoded: Vec<u8>, response_headers: &[ProviderResponseHeader]) -> Response {
    let mut response = Response::new(Body::from(encoded));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    apply_safe_response_headers(response, response_headers)
}

fn apply_safe_response_headers(
    mut response: Response,
    response_headers: &[ProviderResponseHeader],
) -> Response {
    for header in response_headers {
        let Some(name) = super::safe_response_header_name(header.name()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(header.value().as_str()) else {
            continue;
        };
        response
            .headers_mut()
            .insert(axum::http::header::HeaderName::from_static(name), value);
    }
    response
}

/// 编码首个 SSE frame 后提交下游，再持续驱动同一执行会话。
pub async fn stream_execution_response(
    session: Box<dyn ExecutionSession>,
    created_at: u64,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
) -> Response {
    let mut execution = PendingExecution::new(session);
    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let first = match session.next_event().await {
        Ok(Some(event)) => event,
        Ok(None) => {
            let response = gateway_error_response(&GatewayError::new(
                GatewayErrorKind::Internal,
                "gateway response ended before its first event",
            ));
            let response = execution.record_response_status(response).await;
            execution.cancel_and_finalize().await;
            return response;
        }
        Err(error) => {
            let response = gateway_error_response(&gateway_error_from_engine(&error));
            return execution.record_response_status(response).await;
        }
    };
    let first_requirement = first.commit_requirement();
    let first_events = first.into_provider_events();
    if first_requirement != CommitRequirement::CommitBeforeDelivery {
        let response = internal_gateway_response("gateway first event did not require commit");
        let response = execution.record_response_status(response).await;
        execution.cancel_and_finalize().await;
        return response;
    }
    let mut encoder = OpenAiResponsesEncoder::new(created_at);
    let mut frames = Vec::new();
    for event in &first_events {
        match encoder.push_sse(event) {
            Ok(encoded) => frames.extend(encoded),
            Err(error) => {
                let response = protocol_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error.protocol_body(),
                );
                let response = execution.record_response_status(response).await;
                execution.cancel_and_finalize().await;
                return response;
            }
        }
    }
    if frames.is_empty() {
        let response = internal_gateway_response("gateway commit batch encoded no output");
        let response = execution.record_response_status(response).await;
        execution.cancel_and_finalize().await;
        return response;
    }
    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let response_headers = session.response_headers().to_vec();
    if let Err(error) = session
        .commit_downstream(Some(StatusCode::OK.as_u16()))
        .await
    {
        execution.cancel_and_finalize().await;
        let response = gateway_error_response(&gateway_error_from_engine(&error));
        return execution.record_response_status(response).await;
    }
    let Some(session) = execution.into_session() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let mut state = ResponsesStreamState::new(session, encoder, frames, connection_guard);
    if state.encoder.is_completed() {
        state.finish_completed(Vec::new()).await;
    }
    let body = Body::from_stream(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.pending.pop_front() {
                return Some((Ok::<Bytes, Infallible>(chunk), state));
            }
            if state.output_finished {
                return None;
            }
            state.advance().await;
        }
    }));
    event_stream_response(body, &response_headers)
}

fn event_stream_response(body: Body, response_headers: &[ProviderResponseHeader]) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    apply_safe_response_headers(response, response_headers)
}

fn internal_gateway_response(message: &'static str) -> Response {
    gateway_error_response(&GatewayError::new(GatewayErrorKind::Internal, message))
}

pub(super) struct PendingExecution {
    session: Option<Box<dyn ExecutionSession>>,
}

impl PendingExecution {
    pub(super) fn new(session: Box<dyn ExecutionSession>) -> Self {
        Self {
            session: Some(session),
        }
    }

    pub(super) fn session_mut(&mut self) -> Option<&mut (dyn ExecutionSession + 'static)> {
        self.session.as_deref_mut()
    }

    async fn cancel_and_finalize(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.cancel();
            if !session.is_finalized() {
                let _ = session.next_event().await;
            }
        }
    }

    async fn record_response_status(&mut self, response: Response) -> Response {
        let Some(session) = self.session.as_mut() else {
            return internal_gateway_response("gateway response session is unavailable");
        };
        if session
            .record_client_status(response.status().as_u16())
            .await
            .is_err()
        {
            return internal_gateway_response("gateway client status could not be persisted");
        }
        response
    }

    pub(super) fn disarm(&mut self) {
        self.session = None;
    }

    fn into_session(mut self) -> Option<Box<dyn ExecutionSession>> {
        self.session.take()
    }
}

impl Drop for PendingExecution {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        if session.is_finalized() {
            return;
        }
        session.cancel();
        detach_finalize(session);
    }
}

struct ResponsesStreamState {
    session: Option<Box<dyn ExecutionSession>>,
    encoder: OpenAiResponsesEncoder,
    pending: VecDeque<Bytes>,
    output_finished: bool,
    execution_terminal: bool,
    _connection_guard: Option<Box<dyn ConnectionGuard>>,
}

impl ResponsesStreamState {
    fn new(
        session: Box<dyn ExecutionSession>,
        encoder: OpenAiResponsesEncoder,
        initial_frames: Vec<String>,
        connection_guard: Option<Box<dyn ConnectionGuard>>,
    ) -> Self {
        Self {
            session: Some(session),
            encoder,
            pending: initial_frames.into_iter().map(Bytes::from).collect(),
            output_finished: false,
            execution_terminal: false,
            _connection_guard: connection_guard,
        }
    }

    async fn advance(&mut self) {
        let next = match self.session.as_mut() {
            Some(session) => session.next_event().await,
            None => {
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response session is unavailable",
                ));
                return;
            }
        };
        match next {
            Ok(Some(event)) => {
                let requirement = event.commit_requirement();
                let events = event.into_provider_events();
                if requirement != CommitRequirement::AlreadyCommitted {
                    self.cancel_execution().await;
                    self.finish_with_gateway_error(GatewayError::new(
                        GatewayErrorKind::Internal,
                        "gateway requested another downstream commit",
                    ));
                } else {
                    for event in events {
                        self.push_event(event).await;
                        if self.output_finished {
                            break;
                        }
                    }
                }
            }
            Ok(None) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized());
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response ended without a terminal event",
                ));
            }
            Err(error) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized());
                self.finish_with_gateway_error(gateway_error_from_engine(&error));
            }
        }
    }

    async fn push_event(&mut self, event: ProviderEvent) {
        match self.encoder.push_sse(&event) {
            Ok(frames) => {
                if self.encoder.is_completed() {
                    self.finish_completed(frames).await;
                } else {
                    self.pending.extend(frames.into_iter().map(Bytes::from));
                }
            }
            Err(error) => {
                self.cancel_execution().await;
                self.finish_with_encode_error(error);
            }
        }
    }

    async fn finish_completed(&mut self, frames: Vec<String>) {
        let next = match self.session.as_mut() {
            Some(session) => session.next_event().await,
            None => {
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response session is unavailable",
                ));
                return;
            }
        };
        match next {
            Ok(None)
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized()) =>
            {
                self.pending.extend(frames.into_iter().map(Bytes::from));
                self.execution_terminal = true;
                self.pending
                    .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
                self.output_finished = true;
            }
            Ok(None) => {
                self.cancel_execution().await;
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response was not finalized after its terminal event",
                ));
            }
            Ok(Some(_)) => {
                self.cancel_execution().await;
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response continued after its terminal event",
                ));
            }
            Err(error) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized());
                self.finish_with_gateway_error(gateway_error_from_engine(&error));
            }
        }
    }

    async fn cancel_execution(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.cancel();
            if !session.is_finalized() {
                let _ = session.next_event().await;
            }
            self.execution_terminal = session.is_finalized();
        }
    }

    fn finish_with_gateway_error(&mut self, error: GatewayError) {
        let (_, error_type, code) = gateway_error_contract(error.kind());
        self.pending
            .push_back(Bytes::from(response_failed_sse_event(
                error_type,
                code,
                error.safe_message(),
            )));
        self.pending
            .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
        self.output_finished = true;
    }

    fn finish_with_encode_error(&mut self, error: ResponseEncodeError) {
        let protocol = error.protocol_body().error;
        self.pending
            .push_back(Bytes::from(response_failed_sse_event(
                protocol.kind,
                protocol.code,
                &protocol.message,
            )));
        self.pending
            .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
        self.output_finished = true;
    }
}

impl Drop for ResponsesStreamState {
    fn drop(&mut self) {
        if self.execution_terminal {
            return;
        }
        let Some(session) = self.session.take() else {
            return;
        };
        session.cancel();
        // Body drop 后由 Core 继续状态机清理，HTTP body 生命周期不等待它。
        detach_finalize(session);
    }
}

fn detach_finalize(session: Box<dyn ExecutionSession>) {
    let finalize = session.detach_finalize();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        drop(runtime.spawn(finalize));
    }
}
