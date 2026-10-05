//! Codex Images 非流式 HTTP adapter

use std::net::SocketAddr;

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::HeaderMap,
    response::Response,
};
use gateway_core::engine::execution::ClientTransport;
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::operation::{
    ImageRequest, ImageRequestKind, Operation, OperationKind, RawJsonPayload,
};
use serde_json::Value;

use crate::ApiState;
use crate::middleware::headers::encode_headers;
use crate::openai::middleware::RequestInput;
use crate::openai::{
    auth::{authenticate_client, client_access_error_response},
    endpoint::provider_endpoint_response,
    responses::{OpenAiRequestHeaders, request_client_context},
    router::{IMAGE_EDITS_PATH, IMAGE_GENERATIONS_PATH},
};

const OPENAI_PROTOCOL: &str = "openai";
const IMAGE_TURN_ID_CONTEXT_KEY: &str = "image_turn_id";

/// `POST /v1/images/generations`
pub(crate) async fn image_generations(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_image_request(
        state,
        connect_info,
        headers,
        body,
        ImageRequestKind::Generation,
        IMAGE_GENERATIONS_PATH,
    )
    .await
}

/// `POST /v1/images/edits`
pub(crate) async fn image_edits(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_image_request(
        state,
        connect_info,
        headers,
        body,
        ImageRequestKind::Edit,
        IMAGE_EDITS_PATH,
    )
    .await
}

async fn handle_image_request(
    state: ApiState,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
    kind: ImageRequestKind,
    endpoint: &'static str,
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
            endpoint: endpoint.to_owned(),
            protocol: OPENAI_PROTOCOL.to_owned(),
            operation: Some(OperationKind::GenerateImage),
            transport: ClientTransport::HttpJson,
            model_hint: None,
            headers: encode_headers(&headers),
            body,
        },
        client_ip,
        user_agent,
        move |body, headers| image_operation(body, headers, kind),
    )
    .await
}

fn image_operation(
    body: Bytes,
    headers: &HeaderMap,
    kind: ImageRequestKind,
) -> Result<Operation, GatewayError> {
    let mut context = OpenAiRequestHeaders::from_headers(headers).session_context();
    if let Some(turn_id) = headers
        .get("x-codex-image-turn-id")
        .and_then(|value| value.to_str().ok())
    {
        context.insert(
            IMAGE_TURN_ID_CONTEXT_KEY.to_owned(),
            Value::String(turn_id.to_owned()),
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
    Ok(Operation::GenerateImage(ImageRequest::from_raw_json(
        kind, payload,
    )))
}
