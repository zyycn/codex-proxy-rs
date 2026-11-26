//! Chat HTTP 入口保留原始协议，由目标 Provider 直接转换

use std::net::SocketAddr;

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{
        HeaderMap, StatusCode,
        header::{CONTENT_ENCODING, CONTENT_LENGTH},
    },
    response::Response,
};
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_protocol::chat_completions::{self, CHAT_PROTOCOL};

use crate::ApiState;
use crate::data_plane::{
    auth::{ClientKeyHeaders, authenticate_client, client_access_error_response},
    body::{RequestDecodeError, decompress_request_body},
    error::{gateway_error_response, protocol_error_response},
    generation::{
        ContinuationIntent, DecodedGenerationRequest, GenerationEndpoint, GenerationHttpRequest,
        GenerationRequestMetadata, execute_prepared_generation,
    },
    responses::OpenAiRequestHeaders,
};

pub(in crate::data_plane) async fn chat_completions(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.data_plane().clone();
    let client = match authenticate_client(&service, &headers, ClientKeyHeaders::Bearer).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let maximum = client.snapshot().responses_max_decompressed_body_bytes();
    let body = match decompress_request_body(&body, &headers, maximum) {
        Ok(body) => Bytes::copy_from_slice(&body),
        Err(error) => {
            return protocol_error_response(StatusCode::BAD_REQUEST, error.protocol_body());
        }
    };
    headers.remove(CONTENT_ENCODING);
    headers.remove(CONTENT_LENGTH);
    let decoded = match decode_request(&body, &headers, maximum) {
        Ok(decoded) => decoded,
        Err(error) => {
            return protocol_error_response(StatusCode::BAD_REQUEST, error.protocol_body());
        }
    };
    let prepared = match service.execution().prepare_execution(client).await {
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
            input: GenerationEndpoint::Chat.input(decoded),
            middleware_body: body,
        },
        None,
    )
    .await
}

pub(in crate::data_plane) fn decode_request(
    body: &[u8],
    headers: &HeaderMap,
    maximum: usize,
) -> Result<DecodedGenerationRequest, RequestDecodeError> {
    let body = decompress_request_body(body, headers, maximum)?;
    let parsed = chat_completions::parse_request(&body).map_err(decode_error)?;
    let model = parsed
        .body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let stream = parsed
        .body
        .get("stream")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let payload = ProtocolPayload::from_json_bytes(CHAT_PROTOCOL, Bytes::copy_from_slice(&body))
        .map_err(|_| RequestDecodeError::CanonicalContract {
            field: "chat request".to_owned(),
        })?
        .with_context(OpenAiRequestHeaders::from_headers(headers).session_context());
    Ok(DecodedGenerationRequest {
        operation: Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
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

fn decode_error(error: chat_completions::ChatError) -> RequestDecodeError {
    use chat_completions::ChatError;
    match error {
        ChatError::MalformedJson => RequestDecodeError::MalformedJson,
        ChatError::ExpectedObject => RequestDecodeError::ExpectedObject,
        ChatError::MissingField { field } => RequestDecodeError::MissingField { field },
        ChatError::InvalidType { field, expected } => {
            RequestDecodeError::InvalidType { field, expected }
        }
        ChatError::EmptyField { field } => RequestDecodeError::EmptyField { field },
        ChatError::InvalidValue { field } => RequestDecodeError::InvalidValue { field },
        ChatError::UnknownField { field } => RequestDecodeError::UnknownField { field },
        ChatError::UnsupportedField { field } => RequestDecodeError::UnsupportedField { field },
        ChatError::UnsupportedCombination { field, other } => {
            RequestDecodeError::UnsupportedCombination { field, other }
        }
        ChatError::InvalidResponse | ChatError::IncompleteStream => {
            RequestDecodeError::CanonicalContract {
                field: "chat request".to_owned(),
            }
        }
    }
}
