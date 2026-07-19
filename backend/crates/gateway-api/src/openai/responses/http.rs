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
    response::Response,
};
use futures::stream;
use gateway_core::engine::CommitRequirement;
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::GatewayEvent;
use gateway_protocol::openai::sse::{DONE_SSE_FRAME, response_failed_sse_event};

use crate::openai::{
    auth::{authenticate_client, authentication_error_response},
    error::{
        gateway_error_contract, gateway_error_from_engine, gateway_error_response,
        protocol_error_response,
    },
    service::{OpenAiApiState, OpenAiClientService, ResponseExecutionSession, ResponsesTransport},
};

use super::{ProtocolErrorBody, ResponseEncodeError, ResponsesCollector, decode_request};

/// `POST /v1/responses`。
pub async fn responses<S>(
    State(state): State<S>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response
where
    S: OpenAiApiState,
{
    let service = state.openai_client_api();
    let client = match authenticate_client(&service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let decoded = match decode_request(&body) {
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
    let started = match service
        .start_response(client, decoded, ResponsesTransport::Http)
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    let (session, created_at, streaming) = started.into_parts();
    if streaming {
        stream_execution_response(session, created_at).await
    } else {
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
pub async fn collect_execution_response<S>(session: S, created_at: u64) -> Response
where
    S: ResponseExecutionSession,
{
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
    let response = json_body_response(encoded);

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
    events: &[GatewayEvent],
) -> Result<Vec<u8>, BufferedResponseEncodeError> {
    let mut collector = ResponsesCollector::new(created_at);
    for event in events {
        collector
            .push(event)
            .map_err(BufferedResponseEncodeError::Canonical)?;
    }
    let response = collector
        .finish()
        .map_err(BufferedResponseEncodeError::Canonical)?;
    serde_json::to_vec(&response).map_err(|_| BufferedResponseEncodeError::Json)
}

fn json_body_response(encoded: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(encoded));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// 编码首个 SSE frame 后提交下游，再持续驱动同一执行会话。
pub async fn stream_execution_response<S>(session: S, created_at: u64) -> Response
where
    S: ResponseExecutionSession,
{
    let mut execution = PendingExecution::new(session);
    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let first = match session.next_delivery_event().await {
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
    let (first_event, first_requirement) = first.into_parts();
    if first_requirement != CommitRequirement::CommitBeforeDelivery {
        let response = internal_gateway_response("gateway first event did not require commit");
        let response = execution.record_response_status(response).await;
        execution.cancel_and_finalize().await;
        return response;
    }
    let mut collector = ResponsesCollector::new(created_at);
    let frames = match collector.push(&first_event) {
        Ok(frames) => frames,
        Err(error) => {
            let response =
                protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error.protocol_body());
            let response = execution.record_response_status(response).await;
            execution.cancel_and_finalize().await;
            return response;
        }
    };
    if frames.is_empty() {
        let response = internal_gateway_response("gateway first event encoded no SSE frame");
        let response = execution.record_response_status(response).await;
        execution.cancel_and_finalize().await;
        return response;
    }
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
    let Some(session) = execution.into_session() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let state = ResponsesStreamState::new(session, collector, frames);
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
    event_stream_response(body)
}

fn event_stream_response(body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn internal_gateway_response(message: &'static str) -> Response {
    gateway_error_response(&GatewayError::new(GatewayErrorKind::Internal, message))
}

pub(super) struct PendingExecution<S>
where
    S: ResponseExecutionSession,
{
    session: Option<S>,
}

impl<S> PendingExecution<S>
where
    S: ResponseExecutionSession,
{
    pub(super) fn new(session: S) -> Self {
        Self {
            session: Some(session),
        }
    }

    pub(super) fn session_mut(&mut self) -> Option<&mut S> {
        self.session.as_mut()
    }

    async fn cancel_and_finalize(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.cancel();
            if !session.is_finalized() {
                let _ = session.next_delivery_event().await;
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

    fn into_session(mut self) -> Option<S> {
        self.session.take()
    }
}

impl<S> Drop for PendingExecution<S>
where
    S: ResponseExecutionSession,
{
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        if session.is_finalized() {
            return;
        }
        session.cancel();
        session.detach_finalize();
    }
}

struct ResponsesStreamState<S>
where
    S: ResponseExecutionSession,
{
    session: Option<S>,
    collector: ResponsesCollector,
    pending: VecDeque<Bytes>,
    output_finished: bool,
    execution_terminal: bool,
}

impl<S> ResponsesStreamState<S>
where
    S: ResponseExecutionSession,
{
    fn new(session: S, collector: ResponsesCollector, initial_frames: Vec<String>) -> Self {
        Self {
            session: Some(session),
            collector,
            pending: initial_frames.into_iter().map(Bytes::from).collect(),
            output_finished: false,
            execution_terminal: false,
        }
    }

    async fn advance(&mut self) {
        let next = match self.session.as_mut() {
            Some(session) => session.next_delivery_event().await,
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
                let (event, requirement) = event.into_parts();
                if requirement != CommitRequirement::AlreadyCommitted {
                    self.cancel_execution().await;
                    self.finish_with_gateway_error(GatewayError::new(
                        GatewayErrorKind::Internal,
                        "gateway requested another downstream commit",
                    ));
                } else {
                    self.push_event(event).await;
                }
            }
            Ok(None) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(ResponseExecutionSession::is_finalized);
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response ended without a terminal event",
                ));
            }
            Err(error) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(ResponseExecutionSession::is_finalized);
                self.finish_with_gateway_error(gateway_error_from_engine(&error));
            }
        }
    }

    async fn push_event(&mut self, event: GatewayEvent) {
        let completed = matches!(event, GatewayEvent::Completed(_));
        match self.collector.push(&event) {
            Ok(frames) => {
                if completed {
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
            Some(session) => session.next_delivery_event().await,
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
                    .is_some_and(ResponseExecutionSession::is_finalized) =>
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
                    .is_some_and(ResponseExecutionSession::is_finalized);
                self.finish_with_gateway_error(gateway_error_from_engine(&error));
            }
        }
    }

    async fn cancel_execution(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.cancel();
            if !session.is_finalized() {
                let _ = session.next_delivery_event().await;
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

impl<S> Drop for ResponsesStreamState<S>
where
    S: ResponseExecutionSession,
{
    fn drop(&mut self) {
        if self.execution_terminal {
            return;
        }
        let Some(session) = self.session.take() else {
            return;
        };
        session.cancel();
        // Body drop 后由应用 runtime 继续状态机清理，HTTP body 生命周期不等待它。
        session.detach_finalize();
    }
}
