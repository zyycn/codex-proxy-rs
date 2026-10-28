//! Chat HTTP 入口只规范化协议 共用 Responses 的执行与交付链路

use std::{net::SocketAddr, time::SystemTime};

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, StatusCode, header::CONTENT_ENCODING},
    response::Response,
};

use crate::ApiState;
use crate::openai::{
    auth::{authenticate_client, client_access_error_response},
    error::{gateway_error_response, protocol_error_response},
    responses::{
        DecodedResponsesRequest, HttpResponseAdapter, RequestDecodeError, ResponsesHttpRequest,
        decode_request_with_headers, decompress_request_body, execute_prepared_response,
    },
    router::CHAT_COMPLETIONS_PATH,
};

use super::{
    delivery::{chat_error_frame, project_middleware_response},
    request::convert_request,
    response::ChatIdentity,
};

pub(in crate::openai) async fn chat_completions(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai().clone();
    let client = match authenticate_client(&service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let (decoded, middleware_body, include_usage) = match decode_request(
        &body,
        &headers,
        client.snapshot().responses_max_decompressed_body_bytes(),
    ) {
        Ok(request) => request,
        Err(error) => {
            return protocol_error_response(StatusCode::BAD_REQUEST, error.protocol_body());
        }
    };
    let prepared = match service.execution().prepare_execution(client).await {
        Ok(prepared) => prepared,
        Err(error) => return gateway_error_response(&error),
    };
    let identity = ChatIdentity {
        id: format!("chatcmpl-{}", prepared.request_id().as_str()),
        created: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        model: decoded.metadata().requested_model().to_owned(),
    };
    let streaming = decoded.metadata().stream();
    let adapter = HttpResponseAdapter {
        endpoint: CHAT_COMPLETIONS_PATH,
        wrap: Box::new(move |response| {
            project_middleware_response(response, identity, streaming, include_usage)
        }),
        stream_error: chat_error_frame,
    };
    execute_prepared_response(
        service,
        prepared,
        ResponsesHttpRequest {
            peer_address: connect_info.map(|Extension(ConnectInfo(address))| address),
            ingress_id,
            headers,
            decoded,
            middleware_body,
        },
        None,
        Some(adapter),
    )
    .await
}

fn decode_request(
    body: &[u8],
    headers: &HeaderMap,
    maximum_body_bytes: usize,
) -> Result<(DecodedResponsesRequest, Bytes, bool), RequestDecodeError> {
    let body = decompress_request_body(body, headers, maximum_body_bytes)?;
    let converted = convert_request(&body)?;
    let body =
        serde_json::to_vec(&converted.body).map_err(|_| RequestDecodeError::MalformedJson)?;
    let mut decoded_headers = headers.clone();
    decoded_headers.remove(CONTENT_ENCODING);
    let decoded = decode_request_with_headers(&body, &decoded_headers, maximum_body_bytes)?;
    Ok((decoded, Bytes::from(body), converted.include_usage))
}
