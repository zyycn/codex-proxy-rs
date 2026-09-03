//! OpenAI Provider 原生非流式 JSON 端点的公共交付边界。

use axum::{
    body::{Body, Bytes},
    http::{HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use gateway_core::engine::execution::StartedExecution;
use gateway_core::event::{ProviderEvent, ProviderResponseHeader};

use super::{
    error::{engine_error_response, protocol_error_response},
    responses::{PendingExecution, ProtocolError, ProtocolErrorBody},
};

/// 收集一个 Provider 原生 JSON 响应，并在 Core commit 后按原始 bytes 交付。
pub(super) async fn collect_raw_json_response(started: StartedExecution) -> Response {
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
    let connection_options = super::responses::response_connection_options(response_headers);
    for header in response_headers {
        if !super::responses::response_header_is_forwardable(header.name(), &connection_options) {
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
                message: "The gateway could not forward the upstream JSON response.".to_owned(),
                param: None,
            },
        },
    )
    .into_response()
}
