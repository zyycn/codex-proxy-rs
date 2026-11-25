//! Google GenAI 原生 HTTP 入口，模型和生成方法取自 URL，业务正文进入通用执行链路

mod error;
mod models;
pub(super) use error::project_error_response;
pub(in crate::data_plane) use error::{error_frame, stream_error};

use std::{borrow::Cow, net::SocketAddr};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, Extension, Path, RawQuery, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse as _, Response},
    routing::{get, post},
};
use gateway_core::{
    error::{GatewayError, GatewayErrorKind},
    operation::{GenerateRequest, Operation, ProtocolPayload, RawJsonPayload, TokenCountRequest},
};
use gateway_protocol::genai::GENAI_PROTOCOL;
use serde_json::{Map, Value, json};

use crate::{
    ApiState,
    data_plane::{
        auth::{
            ClientKeyHeaders, authenticate_client, client_access_error_response, verify_client,
        },
        body::decompress_request_body,
        error::gateway_error_response,
        generation::{
            ContinuationIntent, DecodedGenerationRequest, GenerationEndpoint,
            GenerationHttpRequest, GenerationInput, GenerationRequestMetadata,
            execute_prepared_generation,
        },
    },
};

pub(super) const MODEL_ACTION_PATH: &str = "/v1beta/models/{resource}";
const METRICS_PATH: &str = "/v1beta:{method}";

pub(super) fn router() -> Router<ApiState> {
    Router::new()
        .route(models::MODELS_PATH, get(models::list))
        .route(MODEL_ACTION_PATH, post(generate).get(models::detail))
        .route(METRICS_PATH, post(metric))
        .layer(axum::middleware::map_response(project_error_response))
}

fn parse_route(resource: &str, query: Option<&str>) -> Result<GenerationInput, GatewayError> {
    let invalid = |message| GatewayError::new(GatewayErrorKind::InvalidRequest, message);
    let (model, method) = resource
        .split_once(':')
        .ok_or_else(|| invalid("A model action is required."))?;
    if !valid_segment(model) {
        return Err(invalid("Invalid model name."));
    }
    let (endpoint, stream) = match method {
        "generateContent" => (GenerationEndpoint::GenAi, false),
        "streamGenerateContent" => (GenerationEndpoint::GenAi, true),
        "countTokens" => (GenerationEndpoint::GenAiTokenCount, false),
        _ => {
            return Err(GatewayError::new(
                GatewayErrorKind::ModelNotFound,
                "Unknown model action.",
            ));
        }
    };
    let mut formats = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .filter_map(|(key, value)| (key == "alt").then_some(value));
    let format = formats.next();
    if formats.next().is_some()
        || (stream && format.as_deref() != Some("sse"))
        || (!stream && format.is_some_and(|value| value != "json"))
    {
        return Err(invalid(
            "Invalid response format for the requested model action.",
        ));
    }
    Ok(GenerationInput {
        endpoint,
        model: model.to_owned(),
        stream,
    })
}

pub(super) fn decode(
    model: &str,
    stream: bool,
    endpoint: GenerationEndpoint,
    body: Bytes,
    headers: &HeaderMap,
) -> Result<DecodedGenerationRequest, GatewayError> {
    let mut context = Map::new();
    context.insert("stream".into(), Value::Bool(stream));
    let mut attribution = Map::new();
    for key in [
        "user-agent",
        "x-goog-api-client",
        "x-http-session-id",
        "x-session-id",
    ] {
        let connection_option = headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|value| key.eq_ignore_ascii_case(value.trim()));
        if !connection_option && let Some(value) = headers.get(key).and_then(|v| v.to_str().ok()) {
            attribution.insert(key.into(), json!(value));
        }
    }
    context.insert("headers".into(), Value::Object(attribution));
    let passthrough = crate::data_plane::responses::request::passthrough_headers(headers);
    if !passthrough.is_empty() {
        context.insert("opaque_request_headers".into(), Value::Array(passthrough));
    }
    let payload = ProtocolPayload::from_json_bytes(GENAI_PROTOCOL, body.clone())
        .map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::InvalidRequest,
                "Request body must be a JSON object.",
            )
        })?
        .with_context(context);
    let operation = if endpoint == GenerationEndpoint::GenAiTokenCount {
        Operation::CountTokens(
            TokenCountRequest::from_raw_json(
                RawJsonPayload::new(GENAI_PROTOCOL, body)
                    .map_err(|_| {
                        GatewayError::new(
                            GatewayErrorKind::InvalidRequest,
                            "Invalid request protocol.",
                        )
                    })?
                    .with_context(payload.context().clone()),
            )
            .requiring_native_protocol(),
        )
    } else {
        Operation::Generate(
            GenerateRequest::from_protocol_payload(payload).requiring_native_protocol(),
        )
    };
    Ok(DecodedGenerationRequest {
        operation,
        metadata: GenerationRequestMetadata {
            requested_model: model.to_owned(),
            stream,
            store: false,
            continuation: ContinuationIntent::None,
            client_ip: None,
            user_agent: None,
        },
    })
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-@".contains(&byte))
}

async fn generate(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    ingress_id: Option<Extension<tower_http::request_id::RequestId>>,
    Path(resource): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let service = state.data_plane().clone();
    let client = match authenticate_client(&service, &headers, ClientKeyHeaders::Bearer).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let route = match parse_route(&resource, query.as_deref()) {
        Ok(route) => route,
        Err(error) => return gateway_error_response(&error),
    };
    let body = match decompress_request_body(
        &body,
        &headers,
        client.snapshot().responses_max_decompressed_body_bytes(),
    ) {
        Ok(Cow::Borrowed(_)) => body,
        Ok(Cow::Owned(bytes)) => Bytes::from(bytes),
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error::payload(
                    StatusCode::BAD_REQUEST,
                    &error.protocol_body().error.message,
                )),
            )
                .into_response();
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
            peer_address: connect_info.map(|Extension(ConnectInfo(addr))| addr),
            ingress_id,
            headers,
            input: route,
            middleware_body: body,
        },
        None,
        None,
    )
    .await
}

async fn metric(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    match verify_client(state.data_plane(), &headers).await {
        Ok(_) => Json(json!({})).into_response(),
        Err(error) => client_access_error_response(error.into()),
    }
}
