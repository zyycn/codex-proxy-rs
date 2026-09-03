//! Codex standalone search 非流式 HTTP adapter。

use std::net::SocketAddr;

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::HeaderMap,
    response::Response,
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::operation::{Operation, RawJsonPayload, StandaloneSearchRequest};
use serde_json::{Map, Value};

use crate::ApiState;
use crate::openai::{
    auth::{authenticate_client, client_access_error_response},
    error::gateway_error_response,
    provider_endpoint::collect_raw_json_response,
    responses::request_client_context,
};

const OPENAI_PROTOCOL: &str = "openai";
const TURN_METADATA_CONTEXT_KEY: &str = "turn_metadata";

/// `POST /v1/alpha/search`。
pub(crate) async fn standalone_search(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.openai();
    let client = match authenticate_client(service, &headers) {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    let operation = match search_operation(body, &headers) {
        Ok(operation) => operation,
        Err(error) => return gateway_error_response(&error),
    };
    let started = match service
        .start_provider_endpoint(client, operation, client_ip, user_agent, "/v1/alpha/search")
        .await
    {
        Ok(started) => started,
        Err(error) => return gateway_error_response(&error),
    };
    collect_raw_json_response(started).await
}

fn search_operation(body: Bytes, headers: &HeaderMap) -> Result<Operation, GatewayError> {
    let mut context = Map::new();
    if let Some(turn_metadata) = headers
        .get("x-codex-turn-metadata")
        .and_then(|value| value.to_str().ok())
    {
        context.insert(
            TURN_METADATA_CONTEXT_KEY.to_owned(),
            Value::String(turn_metadata.to_owned()),
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
    Ok(Operation::Search(StandaloneSearchRequest::from_raw_json(
        payload,
    )))
}
