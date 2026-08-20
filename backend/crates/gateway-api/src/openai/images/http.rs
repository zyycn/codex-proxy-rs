//! Codex Images 非流式 HTTP adapter。

use std::net::SocketAddr;

use axum::{
    body::{Body, Bytes},
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use gateway_core::engine::execution::StartedExecution;
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::{ProviderEvent, ProviderResponseHeader};
use gateway_core::operation::{ImageRequest, ImageRequestKind, Operation, RawJsonPayload};
use serde_json::{Map, Value};

use crate::ApiState;
use crate::openai::{
    auth::{authenticate_client, authentication_error_response},
    error::{engine_error_response, gateway_error_response, protocol_error_response},
    responses::{PendingExecution, ProtocolError, ProtocolErrorBody, request_client_context},
};

const OPENAI_PROTOCOL: &str = "openai";
const IMAGE_TURN_ID_CONTEXT_KEY: &str = "image_turn_id";

/// `POST /v1/images/generations`。
pub(crate) async fn image_generations(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_image_request(
        state,
        connect_info,
        headers,
        body,
        ImageRequestKind::Generation,
        "/v1/images/generations",
    )
    .await
}

/// `POST /v1/images/edits`。
pub(crate) async fn image_edits(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_image_request(
        state,
        connect_info,
        headers,
        body,
        ImageRequestKind::Edit,
        "/v1/images/edits",
    )
    .await
}

async fn handle_image_request(
    state: ApiState,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
    kind: ImageRequestKind,
    endpoint: &'static str,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    let operation = match image_operation(body, &headers, kind) {
        Ok(operation) => operation,
        Err(error) => return gateway_error_response(&error),
    };
    let started = match service
        .start_image(client, operation, client_ip, user_agent, endpoint)
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    collect_image_response(started).await
}

fn image_operation(
    body: Bytes,
    headers: &HeaderMap,
    kind: ImageRequestKind,
) -> Result<Operation, GatewayError> {
    let mut context = Map::new();
    if let Some(turn_id) = headers
        .get("x-codex-image-turn-id")
        .and_then(|value| value.to_str().ok())
    {
        context.insert(
            IMAGE_TURN_ID_CONTEXT_KEY.to_owned(),
            Value::String(turn_id.to_owned()),
        );
    }
    let payload = RawJsonPayload::new(OPENAI_PROTOCOL, body)
        .map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "OpenAI protocol identifier is invalid",
            )
        })?
        .with_context(context);
    Ok(Operation::GenerateImage(ImageRequest::from_raw_json(
        kind, payload,
    )))
}

async fn collect_image_response(started: StartedExecution) -> Response {
    let mut execution = PendingExecution::new(started.session);
    let Some(session) = execution.session_mut() else {
        return invalid_upstream_response();
    };
    let events = match session.collect_uncommitted().await {
        Ok(events) => events,
        Err(error) => {
            let response = engine_error_response(&error);
            return execution.record_response_status(response).await;
        }
    };
    let body = match raw_json_body(events) {
        Some(body) => body,
        None => {
            let response = invalid_upstream_response();
            let response = execution.record_response_status(response).await;
            execution.cancel_and_finalize().await;
            return response;
        }
    };
    let status = session
        .response_status_code()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .filter(StatusCode::is_success)
        .unwrap_or(StatusCode::OK);
    let response_headers = session.response_headers().to_vec();
    let response = json_body_response(body, status, &response_headers);
    let Some(session) = execution.session_mut() else {
        return invalid_upstream_response();
    };
    if let Err(error) = session.commit_downstream(Some(status.as_u16())).await {
        execution.cancel_and_finalize().await;
        let response = engine_error_response(&error);
        return execution.record_response_status(response).await;
    }
    if !session.is_finalized() {
        execution.cancel_and_finalize().await;
        return invalid_upstream_response();
    }
    execution.disarm();
    response
}

fn raw_json_body(events: Vec<ProviderEvent>) -> Option<Bytes> {
    let mut body = None;
    for event in events {
        let (_, wire) = event.into_parts();
        let Some(wire) = wire.filter(|wire| wire.protocol() == "openai") else {
            continue;
        };
        let Some(raw) = wire.into_raw_json_body() else {
            continue;
        };
        if body.replace(raw).is_some() {
            return None;
        }
    }
    body
}

fn json_body_response(
    body: Bytes,
    status: StatusCode,
    response_headers: &[ProviderResponseHeader],
) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    apply_response_headers(response, response_headers)
}

fn apply_response_headers(
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

fn invalid_upstream_response() -> Response {
    protocol_error_response(
        StatusCode::BAD_GATEWAY,
        ProtocolErrorBody {
            error: ProtocolError {
                kind: "server_error",
                code: "invalid_upstream_response",
                message: "The gateway could not forward the upstream image response.".to_owned(),
                param: None,
            },
        },
    )
    .into_response()
}
