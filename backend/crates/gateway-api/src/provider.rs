//! Provider 自有操作的稳定 HTTP adapter

use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{MethodFilter, on, post},
};
use bytes::BytesMut;
use gateway_core::{
    engine::execution::{
        ClientTransport, ExecutionRequestMetadata, ExecutionSession, StartProviderExecution,
        StartedExecution,
    },
    event::{ProviderEvent, ProviderResponseHeader},
    operation::{
        Operation, ProviderHttpHeader, ProviderHttpMethod, ProviderHttpRequest, RawHttpPayload,
        RawJsonPayload, TokenCountRequest,
    },
    routing::{ProviderKind, UpstreamModelId},
};

use crate::{
    ApiState,
    openai::{
        auth::{authenticate_client, client_access_error_response},
        error::{engine_error_response, gateway_error_response, openai_error_response},
        middleware::PendingExecution,
        responses::request_client_context,
        with_model_request_id,
    },
};

const MAXIMUM_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_RAW_BODY_CHUNK_BYTES: usize = 64 * 1024;
const TOKEN_COUNT_PROTOCOL: &str = "token-count";
const PROVIDER_HTTP_PROTOCOL: &str = "provider-http";

pub(crate) fn router() -> Router<ApiState> {
    Router::new()
        .route(
            "/v1/providers/{provider}/models/{model}/count_tokens",
            post(count_tokens),
        )
        // `get()` 会隐式接受 HEAD；这里必须只匹配明确声明的 GET/POST
        .route(
            "/v1/providers/{provider}/http/{endpoint}",
            on(MethodFilter::GET.or(MethodFilter::POST), provider_http).head(provider_http_head),
        )
        .layer(DefaultBodyLimit::max(MAXIMUM_BODY_BYTES))
}

async fn provider_http_head() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

async fn count_tokens(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    Path((provider, model)): Path<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let provider = match ProviderKind::new(provider) {
        Ok(provider) => provider,
        Err(_) => return invalid_request("provider identifier is invalid"),
    };
    let model = match UpstreamModelId::new(model) {
        Ok(model) => model,
        Err(_) => return invalid_request("provider model identifier is invalid"),
    };
    if serde_json::from_slice::<serde::de::IgnoredAny>(&body).is_err() {
        return invalid_request("token count request must be valid JSON");
    }
    let payload = match RawJsonPayload::new(TOKEN_COUNT_PROTOCOL, body) {
        Ok(payload) => payload,
        Err(_) => return invalid_request("token count request is invalid"),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    let started = match service
        .start_bound_provider_endpoint(StartProviderExecution {
            client,
            provider,
            upstream_model: Some(model),
            operation: Operation::CountTokens(TokenCountRequest::from_raw_json(payload)),
            metadata: ExecutionRequestMetadata {
                protocol: TOKEN_COUNT_PROTOCOL.to_owned(),
                endpoint: uri.path().to_owned(),
                transport: ClientTransport::HttpJson,
                stream: false,
                client_ip,
                user_agent,
                previous_response_id: None,
            },
        })
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    collect_provider_response(started, ResponseBodyKind::Json).await
}

async fn provider_http(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    Path((provider, endpoint)): Path<(String, String)>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if method == Method::GET && !body.is_empty() {
        return invalid_request("GET provider HTTP operations cannot carry a request body");
    }
    let method = match method {
        Method::GET => ProviderHttpMethod::Get,
        Method::POST => ProviderHttpMethod::Post,
        _ => return invalid_request("provider HTTP method is unsupported"),
    };
    let service = state.openai();
    let client = match authenticate_client(service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let provider = match ProviderKind::new(provider) {
        Ok(provider) => provider,
        Err(_) => return invalid_request("provider identifier is invalid"),
    };
    let connection_options = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let request_headers = headers
        .iter()
        .filter(|(name, _)| request_header_is_forwardable(name, &connection_options))
        .map(|(name, value)| {
            ProviderHttpHeader::new(name.as_str(), Bytes::copy_from_slice(value.as_bytes()))
        })
        .collect();
    let payload = match RawHttpPayload::new(PROVIDER_HTTP_PROTOCOL, body) {
        Ok(payload) => payload,
        Err(_) => return invalid_request("provider HTTP request is invalid"),
    };
    let operation = match ProviderHttpRequest::new(
        endpoint,
        method,
        uri.query().map(str::to_owned),
        request_headers,
        payload,
    ) {
        Ok(request) => Operation::ProviderHttp(request),
        Err(_) => return invalid_request("provider HTTP request is invalid"),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    let started = match service
        .start_bound_provider_endpoint(StartProviderExecution {
            client,
            provider,
            upstream_model: None,
            operation,
            metadata: ExecutionRequestMetadata {
                protocol: PROVIDER_HTTP_PROTOCOL.to_owned(),
                endpoint: uri.path().to_owned(),
                transport: ClientTransport::HttpJson,
                stream: false,
                client_ip,
                user_agent,
                previous_response_id: None,
            },
        })
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    collect_provider_response(started, ResponseBodyKind::Opaque).await
}

#[derive(Clone, Copy)]
enum ResponseBodyKind {
    Json,
    Opaque,
}

async fn collect_provider_response(started: StartedExecution, kind: ResponseBodyKind) -> Response {
    let request_id = started.request_id;
    let response = collect_provider_session(started.session, kind).await;
    with_model_request_id(response, &request_id)
}

async fn collect_provider_session(
    session: Box<dyn ExecutionSession>,
    kind: ResponseBodyKind,
) -> Response {
    let mut execution = PendingExecution::new(session);
    let Some(session) = execution.session_mut() else {
        return invalid_upstream_response();
    };
    let events = match session.collect_uncommitted().await {
        Ok(events) => events,
        Err(error) => {
            let response = sanitize_response_headers(engine_error_response(&error));
            return execution.record_response_status(response).await;
        }
    };
    let Some(body) = response_body(events, kind) else {
        let response = execution
            .record_response_status(invalid_upstream_response())
            .await;
        execution.cancel_and_finalize().await;
        return response;
    };
    let status = match session.response_status_code() {
        None => StatusCode::OK,
        Some(status) => match StatusCode::from_u16(status) {
            Ok(status) if status.is_success() => status,
            _ => {
                let response = execution
                    .record_response_status(invalid_upstream_response())
                    .await;
                execution.cancel_and_finalize().await;
                return response;
            }
        },
    };
    let response_headers = session.response_headers().to_vec();
    let response = body_response(body, status, &response_headers, kind);
    let Some(session) = execution.session_mut() else {
        return invalid_upstream_response();
    };
    if let Err(error) = session.commit_downstream(Some(status.as_u16())).await {
        execution.cancel_and_finalize().await;
        let response = sanitize_response_headers(engine_error_response(&error));
        return execution.record_response_status(response).await;
    }
    if !session.is_finalized() {
        execution.cancel_and_finalize().await;
        return invalid_upstream_response();
    }
    execution.disarm();
    response
}

fn response_body(events: Vec<ProviderEvent>, kind: ResponseBodyKind) -> Option<Bytes> {
    let expected_protocol = match kind {
        ResponseBodyKind::Json => TOKEN_COUNT_PROTOCOL,
        ResponseBodyKind::Opaque => PROVIDER_HTTP_PROTOCOL,
    };
    let mut body = BytesMut::new();
    let mut raw_body_events = 0usize;
    let mut empty_body_event = false;
    for event in events {
        let (_, wire) = event.into_parts();
        let Some(wire) = wire.filter(|wire| wire.protocol() == expected_protocol) else {
            continue;
        };
        let raw = match kind {
            ResponseBodyKind::Json => wire.into_raw_json_body(),
            ResponseBodyKind::Opaque => wire.into_raw_http_body(),
        }?;
        raw_body_events = raw_body_events.checked_add(1)?;
        if matches!(kind, ResponseBodyKind::Json) && raw_body_events > 1 {
            return None;
        }
        if matches!(kind, ResponseBodyKind::Opaque) && raw.len() > MAXIMUM_RAW_BODY_CHUNK_BYTES {
            return None;
        }
        let total = body.len().checked_add(raw.len())?;
        if total > MAXIMUM_BODY_BYTES
            || empty_body_event
            || (raw.is_empty() && (raw_body_events > 1 || !body.is_empty()))
        {
            return None;
        }
        if raw.is_empty() {
            empty_body_event = true;
        }
        body.extend_from_slice(&raw);
    }
    if raw_body_events == 0 {
        return None;
    }
    let body = body.freeze();
    if matches!(kind, ResponseBodyKind::Json)
        && serde_json::from_slice::<serde::de::IgnoredAny>(&body).is_err()
    {
        return None;
    }
    Some(body)
}

fn body_response(
    body: Bytes,
    status: StatusCode,
    response_headers: &[ProviderResponseHeader],
    kind: ResponseBodyKind,
) -> Response {
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    apply_safe_response_headers(&mut response, response_headers);
    if matches!(kind, ResponseBodyKind::Json) {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    } else if !response.headers().contains_key(header::CONTENT_TYPE) {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
    }
    if let Ok(length) = HeaderValue::from_str(&length.to_string()) {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length);
    }
    response
}

fn apply_safe_response_headers(
    response: &mut Response,
    response_headers: &[ProviderResponseHeader],
) {
    let connection_options = connection_options(response_headers);
    for header in response_headers {
        if !response_header_is_forwardable(header.name(), &connection_options) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.name().as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_bytes(header.value()) else {
            continue;
        };
        if name == header::CONTENT_TYPE {
            response.headers_mut().insert(name, value);
        } else {
            response.headers_mut().append(name, value);
        }
    }
}

fn sanitize_response_headers(mut response: Response) -> Response {
    let connection_options = response
        .headers()
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let blocked = response
        .headers()
        .keys()
        .filter(|name| !response_header_is_forwardable(name.as_str(), &connection_options))
        .cloned()
        .collect::<Vec<_>>();
    for name in blocked {
        response.headers_mut().remove(name);
    }
    response
}

fn connection_options(headers: &[ProviderResponseHeader]) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("connection"))
        .filter_map(|header| std::str::from_utf8(header.value()).ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn request_header_is_forwardable(name: &HeaderName, connection_options: &[String]) -> bool {
    let name = name.as_str();
    !matches!(
        name,
        "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "api-key"
            | "cookie"
            | "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "upgrade"
            | "trailer"
            | "forwarded"
            | "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-proto"
            | "cf-connecting-ip"
            | "x-real-ip"
    ) && !name.starts_with("proxy-")
        && !connection_options.iter().any(|option| option == name)
}

fn response_header_is_forwardable(name: &str, connection_options: &[String]) -> bool {
    let name = name.trim().to_ascii_lowercase();
    !matches!(
        name.as_str(),
        "authorization"
            | "proxy-authenticate"
            | "www-authenticate"
            | "set-cookie"
            | "location"
            | "content-length"
            | "content-encoding"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "upgrade"
            | "trailer"
    ) && !name.starts_with("proxy-")
        && !connection_options.iter().any(|option| option == &name)
}

fn invalid_request(message: &'static str) -> Response {
    openai_error_response(
        StatusCode::BAD_REQUEST,
        message,
        "invalid_request_error",
        "invalid_provider_operation",
    )
    .into_response()
}

fn invalid_upstream_response() -> Response {
    openai_error_response(
        StatusCode::BAD_GATEWAY,
        "The gateway could not forward the provider response.",
        "server_error",
        "invalid_upstream_response",
    )
    .into_response()
}
