//! OpenAI Responses HTTP 与 SSE adapter。

use std::net::{IpAddr, SocketAddr};

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, StatusCode, header::USER_AGENT},
    response::{IntoResponse, Response},
};
use gateway_core::engine::execution::{ClientTransport, StartedExecution};

use crate::ApiState;
use crate::openai::http::{collect_execution_response, stream_execution_response};
use crate::openai::{
    auth::{authenticate_client, client_access_error_response},
    error::{gateway_error_response, protocol_error_response, runtime_unavailable_response},
};

use super::request::decode_request_with_headers;

/// `POST /v1/responses`。
pub(crate) async fn responses(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers) {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let decoded = match decode_request_with_headers(&body, &headers) {
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
            "/v1/responses",
        )
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    let trace = started.session.trace();
    trace.headers("client.request", serde_json::json!({
        "ingressRequestId": ingress_id.as_ref().and_then(|Extension(id)| id.header_value().to_str().ok()),
    }), headers.iter().map(|(name, value)| (name.as_str(), value.as_bytes())));
    trace.capture("client.request.body", &body);
    let request_id = started.request_id;
    let StartedExecution {
        stream, session, ..
    } = started;
    let response = if stream {
        stream_execution_response(session, connection_guard).await
    } else {
        drop(connection_guard);
        collect_execution_response(session).await
    };
    crate::openai::with_model_request_id(response, &request_id)
}

/// 从 socket 与标准转发头提取旧 Usage 页面使用的诊断事实。
pub(in crate::openai) fn request_client_context(
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
