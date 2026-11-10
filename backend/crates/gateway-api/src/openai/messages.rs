//! Messages 与输入计数的 HTTP 入口和原生错误表达

use std::net::SocketAddr;

use axum::{
    body::{Body, Bytes},
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::Response,
};
use gateway_core::{
    error::{GatewayError, GatewayErrorKind},
    operation::{GenerateRequest, Operation, ProtocolPayload, RawJsonPayload, TokenCountRequest},
};
use gateway_protocol::{anthropic, openai::sse::encode_sse_event};
use serde_json::{Value, json};

use super::{
    auth::{ClientKeyHeaders, authenticate_client, client_access_error_response},
    error::gateway_error_response,
    generation::{
        ContinuationIntent, DecodedGenerationRequest, GenerationHttpRequest, GenerationProtocol,
        GenerationRequestMetadata, execute_prepared_generation,
    },
    responses::{OpenAiRequestHeaders, decompress_request_body},
};
use crate::ApiState;

pub(super) async fn messages(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    execute(
        state,
        connect_info,
        ingress_id,
        headers,
        body,
        GenerationProtocol::Messages,
    )
    .await
}

pub(super) async fn count_tokens(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    execute(
        state,
        connect_info,
        ingress_id,
        headers,
        body,
        GenerationProtocol::MessageTokenCount,
    )
    .await
}

async fn execute(
    state: ApiState,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    mut headers: HeaderMap,
    body: Bytes,
    protocol: GenerationProtocol,
) -> Response {
    let service = state.openai().clone();
    let client = match authenticate_client(&service, &headers, ClientKeyHeaders::Anthropic).await {
        Ok(client) => client,
        Err(error) => return project_error_response(client_access_error_response(error)).await,
    };
    let maximum = client.snapshot().responses_max_decompressed_body_bytes();
    let body = match decompress_request_body(&body, &headers, maximum) {
        Ok(body) => Bytes::copy_from_slice(&body),
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &error.protocol_body().error.message,
            );
        }
    };
    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    let decoded = match decode_request(&body, &headers, maximum, protocol) {
        Ok(decoded) => decoded,
        Err(error) => return project_error_response(gateway_error_response(&error)).await,
    };
    let prepared = match service.execution().prepare_execution(client).await {
        Ok(prepared) => prepared,
        Err(error) => return project_error_response(gateway_error_response(&error)).await,
    };
    execute_prepared_generation(
        service,
        prepared,
        GenerationHttpRequest {
            peer_address: connect_info.map(|Extension(ConnectInfo(address))| address),
            ingress_id,
            headers,
            decoded,
            middleware_body: body,
        },
        None,
        None,
    )
    .await
}

pub(super) fn decode_request(
    body: &[u8],
    headers: &HeaderMap,
    maximum: usize,
    protocol: GenerationProtocol,
) -> Result<DecodedGenerationRequest, GatewayError> {
    let invalid = |message| GatewayError::new(GatewayErrorKind::InvalidRequest, message);
    let body = decompress_request_body(body, headers, maximum)
        .map_err(|_| invalid("Request body could not be decoded."))?;
    let object = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| invalid("Request body must be a JSON object."))?;
    let counted = protocol == GenerationProtocol::MessageTokenCount;
    let validation = if counted {
        anthropic::validate_count_request(&object)
    } else {
        anthropic::validate_request(&object)
    };
    validation.map_err(|error| {
        invalid(match error {
            anthropic::TranslationError::InvalidRequest(message) => message,
            _ => "Messages request is invalid.",
        })
    })?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let stream = !counted
        && object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let context = OpenAiRequestHeaders::from_headers(headers).session_context();
    let operation = if counted {
        let payload = RawJsonPayload::new(protocol.wire(), Bytes::copy_from_slice(&body))
            .map_err(|_| invalid("Request protocol is invalid."))?
            .with_context(context);
        Operation::CountTokens(TokenCountRequest::from_raw_json(payload))
    } else {
        let payload = ProtocolPayload::json_object(protocol.wire(), object)
            .map_err(|_| invalid("Request protocol is invalid."))?
            .with_context(context);
        Operation::Generate(GenerateRequest::from_protocol_payload(payload))
    };
    Ok(DecodedGenerationRequest {
        operation,
        metadata: GenerationRequestMetadata {
            requested_model: model,
            stream,
            store: false,
            continuation: ContinuationIntent::None,
            client_ip: None,
            user_agent: None,
        },
    })
}

fn error_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 405 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        _ => "api_error",
    }
}

pub(super) fn error_payload(value: &Value, status: StatusCode) -> Value {
    if value.get("type").and_then(Value::as_str) == Some("error") && value.get("error").is_some() {
        return value.clone();
    }
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .unwrap_or(value);
    // 转接 Responses 的互联拒绝时保留机器码，下一节点需能识别相同的环路
    if gateway_protocol::peer::is_routing_loop(
        Some(status.as_u16()),
        error.get("code").and_then(Value::as_str),
    ) {
        return json!({"type": "error", "error": error});
    }
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("The gateway could not complete the request.");
    json!({"type":"error", "error":{"type":error_type(status), "message":message}})
}

fn error_response(status: StatusCode, kind: &str, message: &str) -> Response {
    let mut response = Response::new(Body::from(
        json!({"type":"error", "error":{"type":kind,"message":message}}).to_string(),
    ));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

pub(super) async fn project_error_response(response: Response) -> Response {
    if response.status().is_success() {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"error":{"message":String::from_utf8_lossy(&bytes)}}));
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let projected = error_payload(&value, parts.status);
    Response::from_parts(parts, Body::from(projected.to_string()))
}

pub(super) fn stream_error(value: &Value) -> Bytes {
    Bytes::from(encode_sse_event(
        "error",
        &error_payload(value, StatusCode::INTERNAL_SERVER_ERROR).to_string(),
    ))
}
