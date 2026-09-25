//! OpenAI Provider 原生非流式 JSON 端点的公共交付边界。

use std::net::IpAddr;

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use gateway_core::{
    engine::{
        execution::{AuthenticatedClient, StartedExecution},
        middleware::{MiddlewareError, MiddlewareFrame, MiddlewareFraming, MiddlewareResponse},
    },
    error::GatewayError,
    operation::Operation,
};

use crate::openai::middleware::{
    ExpectedBody, HttpMiddlewareInput, PendingExecution, buffered_response, error_response,
    into_http_response, invoke_http_middleware, pending_execution_response, request_headers,
    request_parts,
};
use gateway_core::event::{ProviderEvent, ProviderResponseHeader};

use super::{
    error::{engine_error_response, gateway_error_response, protocol_error_response},
    responses::{ProtocolError, ProtocolErrorBody},
    service::OpenAiService,
};

/// Provider 自有端点也先冻结身份，再通过统一请求链进入原有执行与结算路径。
pub(super) async fn provider_endpoint_response<F>(
    service: OpenAiService,
    client: AuthenticatedClient,
    input: HttpMiddlewareInput,
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    decode: F,
) -> Response
where
    F: FnOnce(Bytes, &HeaderMap) -> Result<Operation, GatewayError> + Send + 'static,
{
    let execution = service.execution();
    let prepared = match execution.prepare_execution(client).await {
        Ok(prepared) => prepared,
        Err(error) => return gateway_error_response(&error),
    };
    let request_id = prepared.request_id().clone();
    let endpoint = input.endpoint.clone();
    let result = invoke_http_middleware(
        execution,
        prepared,
        input,
        Box::new(move |prepared, request| {
            Box::pin(async move {
                let (protocol, headers, body) = request_parts(request.clone())?;
                if protocol != "openai" {
                    return Err(MiddlewareError::Rejected);
                }
                let operation = request.apply_capabilities(decode(body, &headers)?)?;
                let started = match service
                    .start_prepared_provider_endpoint(
                        prepared, operation, client_ip, user_agent, endpoint,
                    )
                    .await
                {
                    Ok(started) => started,
                    Err(error) => {
                        return buffered_response("openai", gateway_error_response(&error)).await;
                    }
                };
                collect_raw_json_response(started).await
            })
        }),
    )
    .await;
    let response = match result {
        Ok(response) => into_http_response(response, ExpectedBody::SingleJson).await,
        Err(error) => error_response(error),
    };
    if response.headers().contains_key("x-gateway-request-id") {
        response
    } else {
        super::with_model_request_id(response, &request_id)
    }
}

/// 只收集正文，不提前 commit；外层完成响应变换与校验后才提交 Core。
async fn collect_raw_json_response(
    started: StartedExecution,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let mut execution = PendingExecution::new(started.session);
    let session = execution
        .session_mut()
        .ok_or(MiddlewareError::InvalidState)?;
    let events = session.collect_uncommitted().await;
    let transformed = events
        .as_ref()
        .is_ok_and(|events| events.iter().any(ProviderEvent::middleware_transformed));
    let response = match events {
        Ok(events) => match raw_json_body(events) {
            Some(body) => {
                let status = session
                    .response_status_code()
                    .and_then(|status| StatusCode::from_u16(status).ok())
                    .filter(StatusCode::is_success)
                    .unwrap_or(StatusCode::OK);
                json_body_response(body, status, session.response_headers())
            }
            None => invalid_upstream_response(),
        },
        Err(error) => engine_error_response(&error),
    };
    let response = super::with_model_request_id(response, &started.request_id);
    let (parts, body) = response.into_parts();
    let framing = if parts.status.is_success() {
        MiddlewareFraming::JsonDocument
    } else {
        MiddlewareFraming::RawBytes
    };
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| MiddlewareError::Fault)?;
    Ok(pending_execution_response(
        "openai".to_owned(),
        parts.status.as_u16(),
        request_headers(&parts.headers),
        MiddlewareFrame::new(bytes, framing, true).with_transformed(transformed),
        execution,
    ))
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
