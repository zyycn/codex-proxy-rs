//! Chat Completions HTTP 入站与共享交付生命周期。

use std::net::SocketAddr;

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use gateway_core::engine::execution::ClientTransport;

use crate::ApiState;
use crate::openai::{
    auth::{authenticate_client, client_access_error_response},
    error::{gateway_error_response, protocol_error_response, runtime_unavailable_response},
    http::{collect_chat_response, stream_chat_response},
    responses::request_client_context,
};

use super::{ChatEncoder, request::decode_request_with_headers};

pub(crate) async fn chat_completions(
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
    let model = decoded.responses.metadata().requested_model().to_owned();
    let streaming = decoded.responses.metadata().stream();
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
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
            decoded.responses.with_client_context(client_ip, user_agent),
            if streaming {
                ClientTransport::HttpSse
            } else {
                ClientTransport::HttpJson
            },
            "/v1/chat/completions",
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
    let encoder = ChatEncoder::new(
        &started.request_id,
        started.created_at,
        model,
        decoded.include_usage,
    )
    .with_legacy_functions(decoded.legacy_functions);
    let response = if started.stream {
        stream_chat_response(started.session, connection_guard, encoder).await
    } else {
        drop(connection_guard);
        collect_chat_response(started.session, encoder).await
    };
    crate::openai::with_model_request_id(response, &started.request_id)
}
