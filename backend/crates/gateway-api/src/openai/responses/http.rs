//! Responses HTTP 入口的认证和解码

use super::request::decode_request_with_body;
use crate::{
    ApiState,
    openai::{
        auth::{ClientKeyHeaders, authenticate_client, client_access_error_response},
        error::gateway_error_response,
        generation::{GenerationHttpRequest, execute_prepared_generation},
    },
};
use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use std::net::SocketAddr;

/// `POST /v1/responses`
pub(crate) async fn responses(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai().clone();
    let client = match authenticate_client(&service, &headers, ClientKeyHeaders::Bearer).await {
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
    execute_prepared_generation(
        service,
        prepared,
        GenerationHttpRequest {
            peer_address: connect_info.map(|Extension(ConnectInfo(address))| address),
            ingress_id,
            headers,
            decoded,
            middleware_body,
        },
        None,
        None,
    )
    .await
}
