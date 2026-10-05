//! Live 通话引导（`POST /v1/live`）与 hangup 的 HTTP adapter。

use std::net::SocketAddr;

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Extension, FromRequest, Multipart, Request, State},
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use bytes::{BytesMut, buf::BufMut};
use gateway_core::engine::execution::StartedExecution;
use gateway_core::event::ProviderEvent;
use gateway_core::live::LiveHangupRequest;
use gateway_core::operation::Operation;
use serde_json::Value;

use super::{
    LiveErrorShape, MAX_LIVE_BODY_BYTES, codex_realtime_model, filter_protocol_headers,
    live_call_operation, live_error_response, live_forwarded_response_headers,
    normalize_call_request, rewrite_call_request_model,
};
use crate::{
    ApiState,
    openai::{
        auth::{authenticate_client, client_access_error_response},
        error::{engine_error_response, gateway_error_response},
        middleware::PendingExecution,
        responses::request_client_context,
        with_model_request_id,
    },
};

/// 引导 wire 事件使用的协议名；与 Provider 侧 `PROVIDER_NAME` 一致。
const LIVE_PROTOCOL: &str = "openai";

pub(crate) async fn live_call(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    uri: Uri,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let (body, multipart) = match read_call_body(request, content_type.as_deref()).await {
        Ok(parts) => parts,
        Err(response) => return *response,
    };
    let (upstream_body, upstream_content_type, model) =
        match normalize_call_request(&body, content_type.as_deref(), multipart) {
            Ok(parts) => parts,
            Err(response) => return *response,
        };
    let upstream_model = codex_realtime_model(model);
    let upstream_body = rewrite_call_request_model(upstream_body, &upstream_model);
    // 实际语音模型参与路由与账号权限检查：账号范围不含该模型时在路由层拒绝，
    // 而不是写进正文后仍由未授权账号服务。
    let upstream_model_id =
        match gateway_core::routing::UpstreamModelId::from_client_wire(upstream_model.clone()) {
            Ok(model) => model,
            Err(_) => {
                return live_error_response(
                    LiveErrorShape::Live,
                    StatusCode::BAD_REQUEST,
                    "Codex live call request has an invalid model",
                    "invalid_request",
                );
            }
        };
    let operation = match live_call_operation(
        upstream_body,
        upstream_content_type.as_deref(),
        filter_protocol_headers(&headers),
    ) {
        Ok(operation) => operation,
        Err(response) => return *response,
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    let prepared = match service.execution().prepare_execution(client).await {
        Ok(prepared) => prepared,
        Err(error) => return gateway_error_response(&error),
    };
    let started = match service
        .start_prepared_provider_endpoint(
            prepared,
            Operation::ProviderHttp(operation),
            Some(upstream_model_id),
            client_ip,
            user_agent,
            uri.path().to_owned(),
        )
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    collect_live_call_response(started, uri.path().to_owned()).await
}

pub(crate) async fn hangup(
    State(state): State<ApiState>,
    axum::extract::Path(call_id): axum::extract::Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let Some(gateway) = service.live_gateway() else {
        return super::realtime_unsupported_response("Realtime hangup");
    };
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let protocol_headers = protocol_header_pairs(&headers);
    let outcome = gateway
        .hangup(LiveHangupRequest {
            call_id: &call_id,
            client_api_key_id: client.policy().key_id(),
            content_type,
            body,
            protocol_headers,
        })
        .await;
    match outcome {
        Ok(outcome) => {
            let status = StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::OK);
            let mut response = Response::new(axum::body::Body::from(outcome.body));
            *response.status_mut() = status;
            for (name, value) in outcome.headers {
                if let (Ok(name), Ok(value)) = (
                    header::HeaderName::from_bytes(name.as_bytes()),
                    header::HeaderValue::from_bytes(value.as_bytes()),
                ) {
                    response.headers_mut().insert(name, value);
                }
            }
            response
        }
        Err(error) => live_gateway_error_response(LiveErrorShape::Realtime, &error),
    }
}

/// LiveGateway 错误投影：上游显式状态与正文优先，其余用稳定错误码。
pub(crate) fn live_gateway_error_response(
    shape: LiveErrorShape,
    error: &gateway_core::live::LiveGatewayError,
) -> Response {
    let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    if let Some(body) = error.body() {
        let mut response = Response::new(axum::body::Body::from(body.clone()));
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        return response;
    }
    live_error_response(shape, status, error.message(), error.kind().code())
}

/// 读取引导正文；multipart 入口在此拆成 `(sdp, session)` 字段。
async fn read_call_body(
    request: Request,
    content_type: Option<&str>,
) -> Result<(Bytes, Option<(String, Option<Value>)>), Box<Response>> {
    let is_multipart = content_type
        .and_then(|value| value.split(';').next())
        .map(|media| media.trim().eq_ignore_ascii_case("multipart/form-data"))
        .unwrap_or(false);
    if !is_multipart {
        let body = axum::body::to_bytes(request.into_body(), MAX_LIVE_BODY_BYTES)
            .await
            .map_err(|_| {
                Box::new(live_error_response(
                    LiveErrorShape::Live,
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Codex live request body too large",
                    "invalid_request",
                ))
            })?;
        return Ok((body, None));
    }
    let mut multipart = Multipart::from_request(request, &()).await.map_err(|_| {
        Box::new(live_error_response(
            LiveErrorShape::Live,
            StatusCode::BAD_REQUEST,
            "Codex live multipart body is invalid",
            "invalid_request",
        ))
    })?;
    let mut sdp = None;
    let mut session = None;
    loop {
        let Some(field) = multipart.next_field().await.map_err(|_| {
            Box::new(live_error_response(
                LiveErrorShape::Live,
                StatusCode::BAD_REQUEST,
                "Codex live multipart body is invalid",
                "invalid_request",
            ))
        })?
        else {
            break;
        };
        match field.name() {
            Some("sdp") => {
                let text = field.text().await.map_err(|_| {
                    Box::new(live_error_response(
                        LiveErrorShape::Live,
                        StatusCode::BAD_REQUEST,
                        "Codex live multipart body is invalid",
                        "invalid_request",
                    ))
                })?;
                sdp = Some(text);
            }
            Some("session") => {
                let bytes = field.bytes().await.map_err(|_| {
                    Box::new(live_error_response(
                        LiveErrorShape::Live,
                        StatusCode::BAD_REQUEST,
                        "Codex live multipart body is invalid",
                        "invalid_request",
                    ))
                })?;
                let parsed = serde_json::from_slice::<Value>(&bytes).map_err(|_| {
                    Box::new(live_error_response(
                        LiveErrorShape::Live,
                        StatusCode::BAD_REQUEST,
                        "Codex live session field must contain valid JSON",
                        "invalid_request",
                    ))
                })?;
                session = Some(parsed);
            }
            _ => {}
        }
    }
    match sdp {
        Some(sdp) => Ok((Bytes::new(), Some((sdp, session)))),
        None => Err(Box::new(live_error_response(
            LiveErrorShape::Live,
            StatusCode::BAD_REQUEST,
            "Codex live multipart body requires an sdp field",
            "invalid_request",
        ))),
    }
}

/// realtime 协议头的 `(name, value)` 形态；名称小写化以稳定上游顺序。
pub(crate) fn protocol_header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    super::filter_protocol_headers(headers)
        .into_iter()
        .map(|header| {
            (
                header.name().to_ascii_lowercase(),
                String::from_utf8_lossy(header.value()).into_owned(),
            )
        })
        .collect()
}

/// 汇集引导响应；只放行白名单响应头，正文按字节透传。
async fn collect_live_call_response(started: StartedExecution, path: String) -> Response {
    let request_id = started.request_id;
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
    let Some(body) = live_call_body(events) else {
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
    let mut response = Response::new(axum::body::Body::from(body));
    *response.status_mut() = status;
    for (name, value) in live_forwarded_response_headers(session.response_headers()) {
        if name == header::CONTENT_TYPE {
            response.headers_mut().insert(name, value);
        } else {
            response.headers_mut().append(name, value);
        }
    }
    rewrite_realtime_location(&mut response, &path);
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
    with_model_request_id(response, &request_id)
}

/// `/v1/realtime*` 家族的 `Location` 改写为网关自身路径，保持客户端可回环。
fn rewrite_realtime_location(response: &mut Response, path: &str) {
    if !path.starts_with("/v1/realtime") {
        return;
    }
    let Some(location) = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
    else {
        return;
    };
    let Some(call_id) = gateway_core::live::call_id_from_location(&location) else {
        return;
    };
    if let Ok(value) = header::HeaderValue::from_str(&format!("/v1/realtime/calls/{call_id}")) {
        response.headers_mut().insert(header::LOCATION, value);
    }
}

fn live_call_body(events: Vec<ProviderEvent>) -> Option<Bytes> {
    let mut body = BytesMut::new();
    let mut raw_events = 0usize;
    for event in events {
        let (_, wire) = event.into_parts();
        let Some(wire) = wire.filter(|wire| wire.protocol() == LIVE_PROTOCOL) else {
            continue;
        };
        let raw = wire.into_raw_http_body()?;
        raw_events += 1;
        if raw_events > 1 || body.len() + raw.len() > MAX_LIVE_BODY_BYTES {
            return None;
        }
        body.put_slice(&raw);
    }
    (raw_events == 1).then(|| body.freeze())
}

fn invalid_upstream_response() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        "The gateway could not forward the Codex live response.",
    )
        .into_response()
}
