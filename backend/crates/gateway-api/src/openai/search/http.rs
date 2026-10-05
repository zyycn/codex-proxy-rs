//! Codex standalone search 非流式 HTTP adapter

use std::net::SocketAddr;

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::HeaderMap,
    response::Response,
};
use gateway_core::engine::execution::ClientTransport;
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::operation::{Operation, OperationKind, RawJsonPayload, StandaloneSearchRequest};

use crate::ApiState;
use crate::middleware::headers::encode_headers;
use crate::openai::middleware::RequestInput;
use crate::openai::{
    auth::{authenticate_client, client_access_error_response},
    endpoint::provider_endpoint_response,
    responses::{OpenAiRequestHeaders, request_client_context},
    router::SEARCH_PATH,
};

const OPENAI_PROTOCOL: &str = "openai";

/// `POST /v1/alpha/search`
pub(crate) async fn standalone_search(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    provider_endpoint_response(
        service.clone(),
        client,
        RequestInput {
            endpoint: SEARCH_PATH.to_owned(),
            protocol: OPENAI_PROTOCOL.to_owned(),
            operation: Some(OperationKind::Search),
            transport: ClientTransport::HttpJson,
            model_hint: None,
            headers: encode_headers(&headers),
            body,
        },
        client_ip,
        user_agent,
        search_operation,
    )
    .await
}

fn search_operation(body: Bytes, headers: &HeaderMap) -> Result<Operation, GatewayError> {
    let context = OpenAiRequestHeaders::from_headers(headers).session_context();
    let payload = RawJsonPayload::new(OPENAI_PROTOCOL, body)
        .map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "OpenAI protocol identifier is invalid",
            )
        })?
        .with_context(context);
    Ok(Operation::Search(StandaloneSearchRequest::from_raw_json(
        payload,
    )))
}
